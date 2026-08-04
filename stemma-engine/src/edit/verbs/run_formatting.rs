//! `SetRunFormatting` — author a tracked run-formatting change (`w:rPrChange`,
//! §17.13.5.31). "Bold this defined term, tracked."
//!
//! Reference verb for the `edit/verbs/<verb>.rs` pattern (see `edit/AGENTS.md`).
//! It adds authoring grammar only: it records the run's previous rPr in a
//! `FormattingChange` and applies the new marks. It does **not** touch the
//! materializer (Invariant M) — a formatting change is an in-place rPr delta,
//! not a segment insert/delete, so it bypasses segment lowering entirely. The
//! existing accept/reject projection already resolves `formatting_change`
//! (`tracked_model.rs`: accept keeps the new marks and clears the change;
//! reject restores `previous_marks`/`previous_style_props`).
//!
//! It also carries value-bearing run-style properties (`RunStyleEdit`): color,
//! highlight, font family, font size, and character spacing (`w:spacing`
//! @w:val, §17.3.2.35). Plus the tri-state display marks `w:caps` (§17.3.2.5)
//! and `w:smallCaps` (§17.3.2.33), which ride the boolean `InlineMarkSet` but
//! resolve to `StyleProps` tri-states (like `strike`), not `Mark` variants.
//! These all map 1:1 onto existing `StyleProps` fields and ride the SAME
//! `build_rpr()` + accept/reject machinery as the boolean marks — no new
//! serialization, exactly the lift the verb already does for bold/italic.
//! "Make this defined term red and small-caps, tracked."
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - top-level paragraphs only;
//! - the `expect` span must lie within a single contiguous run of text (it may
//!   not cross an opaque inline or hard break);
//! - the covered runs must not already carry a tracked formatting change;
//! - color is a literal 6-hex RGB or `"auto"` (no theme color refs);
//! - font_family sets only the ascii/hAnsi slot (no east-asia / cs / theme);
//! - a 0 half-point font size is refused.

use super::super::{EditError, InlineMarkSet, MaterializationMode, RunStyleEdit};
use super::super::{
    block_at, block_at_mut, block_id_of, check_ancestor_table_tracking, find_paragraph_path,
};
use crate::domain::{
    BlockNode, CanonDoc, FormattingChange, InlineNode, Mark, MarkValue, NodeId, ParagraphNode,
    RevisionInfo, TextNode, TrackingStatus,
};
use crate::semantic_hash::check_block_guard;

/// A located formatting target: TextNodes `[first..=last]` (contiguous in the
/// segment's inline vector) whose concatenated text contains the match at
/// char range `[start, end)`.
struct SpanPlan {
    first: usize,
    last: usize,
    start: usize,
    end: usize,
}

struct TargetPlan {
    segment_index: usize,
    span: SpanPlan,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    marks: InlineMarkSet,
    style: &RunStyleEdit,
    revision: &RevisionInfo,
    transaction_floor: u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    if marks.is_empty() && style.is_empty() {
        return Err(EditError::NoFormattingRequested { step_index });
    }
    if marks.has_conflict() {
        return Err(EditError::ConflictingFormattingMarks { step_index });
    }

    // Validate value-bearing properties at the verb edge, before mutating
    // anything — no best-effort coercion (CLAUDE.md "no silent fallbacks").
    if let Some(color) = &style.color
        && !is_valid_color(color)
    {
        return Err(EditError::InvalidColorValue {
            value: color.to_string(),
            step_index,
        });
    }
    if style.font_size_half_points == Some(0) {
        return Err(EditError::InvalidFontSize { step_index });
    }

    // Resolve the target paragraph anywhere — top-level OR inside a table cell
    // (find_paragraph_path recurses into cells). In-cell formatting gates the same
    // way as a text replace: the block must be editable, and no enclosing row/cell
    // may be tracked-inserted/deleted.
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    if path.is_top_level() {
        validate_top_level_block_status(&doc.blocks[path.top_block], step_index)?;
    } else {
        check_ancestor_table_tracking(doc, &path, block_id, step_index)?;
    }

    // Block kind + staleness guard (immutable borrow, released before the apply).
    {
        let block = block_at(doc, &path);
        match block {
            BlockNode::Paragraph(_) => {}
            BlockNode::Table(_) => {
                return Err(EditError::NotAParagraph {
                    block_id: block_id.clone(),
                    actual_kind: "table",
                    step_index,
                });
            }
            BlockNode::OpaqueBlock(_) => {
                return Err(EditError::NotAParagraph {
                    block_id: block_id.clone(),
                    actual_kind: "opaque_block",
                    step_index,
                });
            }
        }
        if let Some(expected) = semantic_hash
            && let Err(actual) = check_block_guard(block, expected)
        {
            return Err(EditError::BlockSemanticHashMismatch {
                block_id: block_id.clone(),
                expected: expected.to_string(),
                actual,
                step_index,
            });
        }
    }

    let BlockNode::Paragraph(para) = block_at_mut(doc, &path) else {
        unreachable!("checked paragraph above");
    };
    apply_to_paragraph(
        para,
        block_id,
        expect,
        marks,
        style,
        revision,
        transaction_floor,
        mode,
        step_index,
    )
}

fn validate_top_level_block_status(
    block: &crate::domain::TrackedBlock,
    step_index: usize,
) -> Result<(), EditError> {
    let (status, block_id) = match &block.status {
        TrackingStatus::Normal => return Ok(()),
        TrackingStatus::Inserted(_) => ("inserted", block_id_of(&block.block).clone()),
        TrackingStatus::Deleted(_) => ("deleted", block_id_of(&block.block).clone()),
        TrackingStatus::InsertedThenDeleted(_) => {
            ("inserted_then_deleted", block_id_of(&block.block).clone())
        }
    };
    Err(EditError::BlockHasTrackedStatus {
        block_id,
        status,
        step_index,
    })
}

#[allow(clippy::too_many_arguments)]
fn apply_to_paragraph(
    para: &mut ParagraphNode,
    block_id: &NodeId,
    expect: &str,
    marks: InlineMarkSet,
    style: &RunStyleEdit,
    revision: &RevisionInfo,
    transaction_floor: u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    let matches: Vec<TargetPlan> = para
        .segments
        .iter()
        .enumerate()
        .flat_map(|(segment_index, segment)| {
            find_spans(&segment.inlines, expect)
                .into_iter()
                .map(move |span| TargetPlan {
                    segment_index,
                    span,
                })
        })
        .collect();

    if matches.len() > 1 {
        return Err(EditError::AmbiguousFormatTarget {
            block_id: block_id.clone(),
            expected: expect.to_string(),
            occurrences: matches.len(),
            step_index,
        });
    }

    if let Some(target) = matches.into_iter().next() {
        let segment = &para.segments[target.segment_index];
        let existing_revisions = formatting_changes_in_span(&segment.inlines, &target.span);
        let (tracking_status, container_revision) = tracking_context(&segment.status);

        if !existing_revisions.is_empty() {
            let conflict = existing_revisions.iter().find(|existing_revision| {
                mode == MaterializationMode::Direct
                    || segment.status != TrackingStatus::Normal
                    || existing_revision.identity != 0
                    || existing_revision.revision_id < transaction_floor
            });
            if let Some(existing_revision) = conflict {
                return Err(EditError::FormatRevisionConflict {
                    block_id: block_id.clone(),
                    text: expect.to_string(),
                    tracking_status,
                    existing_revision: Box::new((*existing_revision).clone()),
                    container_revision: Box::new(container_revision),
                    step_index,
                });
            }
        } else if segment.status != TrackingStatus::Normal {
            // Word preserves a first rPrChange inside pending inserted text as
            // an independently attributed, independently resolvable proposal.
            // Author exactly that proven shape. Direct mode still refuses: it
            // would alter text that exists only inside an unresolved proposal
            // without recording a separately reviewable intention. Deleted and
            // stacked text remain outside the v0.5 authoring contract.
            let first_format_on_pending_insert = mode == MaterializationMode::TrackedChange
                && matches!(segment.status, TrackingStatus::Inserted(_));
            if !first_format_on_pending_insert {
                return Err(EditError::FormatTargetNotEditable {
                    block_id: block_id.clone(),
                    text: expect.to_string(),
                    tracking_status,
                    container_revision: Box::new(container_revision),
                    step_index,
                });
            }
        }

        return apply_span(
            &mut para.segments[target.segment_index].inlines,
            block_id,
            target.span,
            marks,
            style,
            revision,
            mode,
            step_index,
        );
    }

    let visible: String = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    Err(EditError::ExpectMismatch {
        block_id: block_id.clone(),
        expected: expect.to_string(),
        actual_text: visible,
        step_index,
    })
}

/// Find `expect` within a single contiguous run of TextNodes. Returns the
/// inline-index range and the char offsets of the match inside that run.
fn find_spans(inlines: &[InlineNode], expect: &str) -> Vec<SpanPlan> {
    if expect.is_empty() {
        return Vec::new();
    }
    let mut plans = Vec::new();
    let mut i = 0;
    while i < inlines.len() {
        if !matches!(inlines[i], InlineNode::Text(_)) {
            i += 1;
            continue;
        }
        // Contiguous TextNode run [i, j).
        let mut j = i;
        let mut concat = String::new();
        while j < inlines.len() {
            match &inlines[j] {
                InlineNode::Text(t) => {
                    concat.push_str(&t.text);
                    j += 1;
                }
                _ => break,
            }
        }
        for (start, end) in char_find_all(&concat, expect) {
            plans.push(SpanPlan {
                first: i,
                last: j - 1,
                start,
                end,
            });
        }
        i = j.max(i + 1);
    }
    plans
}

fn formatting_changes_in_span<'a>(
    inlines: &'a [InlineNode],
    plan: &SpanPlan,
) -> Vec<&'a FormattingChange> {
    let mut changes = Vec::new();
    let mut offset = 0usize;
    for inline in &inlines[plan.first..=plan.last] {
        let InlineNode::Text(node) = inline else {
            unreachable!("span contains text nodes only");
        };
        let end = offset + node.text.chars().count();
        let intersects = plan.start < end && plan.end > offset;
        if intersects && let Some(change) = &node.formatting_change {
            changes.push(change);
        }
        offset = end;
    }
    changes
}

fn tracking_context(status: &TrackingStatus) -> (&'static str, Option<RevisionInfo>) {
    match status {
        TrackingStatus::Normal => ("normal", None),
        TrackingStatus::Inserted(revision) => ("inserted", Some(revision.clone())),
        TrackingStatus::Deleted(revision) => ("deleted", Some(revision.clone())),
        TrackingStatus::InsertedThenDeleted(stacked) => {
            ("inserted_then_deleted", Some(stacked.deleted.clone()))
        }
    }
}

/// A valid w:color literal is the keyword `auto` or exactly six hex digits
/// (§17.18.79 `ST_HexColor` / `ST_HexColorAuto`). We refuse anything else
/// rather than coerce — no `#` prefix, no 3-digit shorthand, no named colors.
fn is_valid_color(value: &str) -> bool {
    value == "auto" || (value.len() == 6 && value.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Byte `find` mapped to a `[start, end)` char-offset range.
fn char_find_all(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    haystack
        .match_indices(needle)
        .map(|(byte, matched)| {
            let start = haystack[..byte].chars().count();
            (start, start + matched.chars().count())
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn apply_span(
    inlines: &mut Vec<InlineNode>,
    block_id: &NodeId,
    plan: SpanPlan,
    marks: InlineMarkSet,
    style: &RunStyleEdit,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // A run that already carries a tracked formatting change can be formatted
    // AGAIN: apply_marks merges the new properties onto the live run while keeping
    // the FIRST change's snapshot as the reject-all baseline, so "make it bigger,
    // then recolor it" composes into one rPrChange that still reverts to the
    // original (B6). (block_id/step_index are no longer needed here.)
    let before = inlines.clone();
    let mut changed = false;

    // Rebuild the covered run, splitting boundary runs so the formatted span is
    // exactly [start, end). Pieces outside the span keep their original rPr.
    let mut rebuilt: Vec<InlineNode> = Vec::new();
    let mut offset = 0usize;
    for inline in inlines.drain(plan.first..=plan.last) {
        let InlineNode::Text(node) = inline else {
            unreachable!("run is all TextNodes");
        };
        let node_len = node.text.chars().count();
        let node_start = offset;
        let node_end = offset + node_len;
        offset = node_end;

        let isect_start = plan.start.max(node_start);
        let isect_end = plan.end.min(node_end);
        if isect_start >= isect_end {
            rebuilt.push(InlineNode::Text(node)); // untouched
            continue;
        }

        let local_s = isect_start - node_start;
        let local_e = isect_end - node_start;
        let chars: Vec<char> = node.text.chars().collect();
        let before: String = chars[..local_s].iter().collect();
        let inside: String = chars[local_s..local_e].iter().collect();
        let after: String = chars[local_e..].iter().collect();
        let oid = node.id.clone();

        if !before.is_empty() {
            let mut b = node.clone();
            b.id = NodeId::new(format!("{oid}_pre"));
            b.text = before;
            rebuilt.push(InlineNode::Text(b));
        }

        let mut mid = node.clone(); // keeps the original id
        mid.text = inside;
        changed |= apply_marks(&mut mid, marks, style, revision, mode);
        rebuilt.push(InlineNode::Text(mid));

        if !after.is_empty() {
            let mut a = node.clone();
            a.id = NodeId::new(format!("{oid}_post"));
            a.text = after;
            rebuilt.push(InlineNode::Text(a));
        }
    }

    // Splice the rebuilt pieces back where the run was (drain left a gap at
    // `plan.first`).
    let tail = inlines.split_off(plan.first);
    inlines.extend(rebuilt);
    inlines.extend(tail);
    if !changed {
        // D5: report what the target ALREADY reads. The caller asked for a
        // state the run is already in; naming that state is what lets them
        // drop the step or fix the half they got wrong.
        let current_formatting = describe_run_formatting(&before);
        *inlines = before;
        return Err(EditError::NoFormattingChange {
            block_id: block_id.clone(),
            step_index,
            current_formatting,
        });
    }
    Ok(())
}

fn ensure_mark(marks: &mut Vec<Mark>, mark: Mark) {
    // Insert in canonical (declaration) order rather than appending: marks are
    // a set, and import always emits them in that order, so appending would
    // leave the model holding an ordering no reopened document can reproduce.
    if let Err(position) = marks.binary_search(&mark) {
        marks.insert(position, mark);
    }
}

fn apply_marks(
    node: &mut TextNode,
    marks: InlineMarkSet,
    style: &RunStyleEdit,
    revision: &RevisionInfo,
    mode: MaterializationMode,
) -> bool {
    // Two distinct "before" snapshots:
    //  - live_*: the run's state right now, used only to detect a true no-op.
    //  - baseline_*: what reject-all must restore. If the run already carries a
    //    tracked formatting change, keep THAT change's snapshot (the real
    //    original) rather than the intermediate live state — otherwise stacking a
    //    second format would make reject-all revert only to the first format, not
    //    the original (B6).
    let live_marks = node.marks.clone();
    let live_style_props = node.style_props.clone();
    let previous_directness = node.rpr_authored;
    // Same "keep the first change's snapshot" rule as marks/style_props
    // above, applied to provenance: a reject must restore what was authored
    // BEFORE the first stacked edit, not the intermediate state.
    let (baseline_marks, baseline_style_props, baseline_rpr_authored) =
        match &node.formatting_change {
            Some(fc) => (
                fc.previous_marks.clone(),
                fc.previous_style_props.clone(),
                fc.previous_rpr_authored,
            ),
            // Record only what the run itself AUTHORED. Word's `w:rPrChange`
            // holds the previous DIRECT `rPr` — adjudicated: formatting a run
            // whose italic comes from its paragraph style writes
            // `<w:rPrChange><w:rPr/></w:rPrChange>`, with no `<w:i/>` in it.
            //
            // Capturing the effective values instead stores history the wire
            // cannot express: the serializer filters this snapshot through its
            // own provenance on emission, so the inherited slots are dropped on
            // save and absent on reopen. That left the same document with two
            // in-memory shapes — effective before a save, direct after — and a
            // reject before the save would have materialized style-inherited
            // formatting onto the run, silently cutting it loose from its style.
            None => (
                crate::serialize::direct_marks(&live_marks, previous_directness),
                crate::serialize::direct_run_style_props(&live_style_props, previous_directness),
                previous_directness,
            ),
        };

    // Each mark the edit sets is DIRECT run formatting: record provenance so
    // the serializer emits it (unauthored marks are filtered — direct_marks).
    if marks.bold {
        ensure_mark(&mut node.marks, Mark::Bold);
        node.rpr_authored.bold = true;
        node.rpr_authored.bold_off = false;
    } else if marks.bold_off {
        node.marks.retain(|mark| *mark != Mark::Bold);
        node.rpr_authored.bold = true;
        node.rpr_authored.bold_off = true;
    }
    if marks.italic {
        ensure_mark(&mut node.marks, Mark::Italic);
        node.rpr_authored.italic = true;
        node.rpr_authored.italic_off = false;
    } else if marks.italic_off {
        node.marks.retain(|mark| *mark != Mark::Italic);
        node.rpr_authored.italic = true;
        node.rpr_authored.italic_off = true;
    }
    if marks.underline {
        ensure_mark(&mut node.marks, Mark::Underline);
        node.rpr_authored.underline = true;
        node.rpr_authored.underline_off = false;
    } else if marks.underline_off {
        node.marks.retain(|mark| *mark != Mark::Underline);
        node.rpr_authored.underline = true;
        node.rpr_authored.underline_off = true;
    }
    if marks.subscript {
        node.marks.retain(|mark| *mark != Mark::Superscript);
        ensure_mark(&mut node.marks, Mark::Subscript);
        node.rpr_authored.vert_align = true;
    } else if marks.subscript_off {
        node.marks.retain(|mark| *mark != Mark::Subscript);
        node.rpr_authored.vert_align = true;
    }
    if marks.superscript {
        node.marks.retain(|mark| *mark != Mark::Subscript);
        ensure_mark(&mut node.marks, Mark::Superscript);
        node.rpr_authored.vert_align = true;
    } else if marks.superscript_off {
        node.marks.retain(|mark| *mark != Mark::Superscript);
        node.rpr_authored.vert_align = true;
    }
    // `strike` is a tri-state style prop, not a boolean Mark.
    if marks.strike {
        node.style_props.strike = MarkValue::On;
        node.rpr_authored.strike = true;
    } else if marks.strike_off {
        node.style_props.strike = MarkValue::Off;
        node.rpr_authored.strike = true;
    }
    // `caps`/`smallCaps` are tri-state style props too (w:caps §17.3.2.5,
    // w:smallCaps §17.3.2.33), not `Mark` enum variants. Setting one turns it
    // `On`; we never flip the other off (additive, like the booleans above).
    if marks.caps {
        node.style_props.caps = MarkValue::On;
        node.rpr_authored.caps = true;
    } else if marks.caps_off {
        node.style_props.caps = MarkValue::Off;
        node.rpr_authored.caps = true;
    }
    if marks.small_caps {
        node.style_props.small_caps = MarkValue::On;
        node.rpr_authored.small_caps = true;
    } else if marks.small_caps_off {
        node.style_props.small_caps = MarkValue::Off;
        node.rpr_authored.small_caps = true;
    }

    // Value-bearing properties (color/highlight/font). v1 sets only the literal
    // color and the ascii/hAnsi font slot; setting a literal color clears any
    // pre-existing theme-color ref so the two never disagree (§17.3.2.6).
    // Setting a value-bearing prop authors it as DIRECT run formatting: record
    // provenance so the serializer emits it (and so it shadows the style
    // cascade, as the user intended). Props the edit doesn't touch keep their
    // existing direct-ness — we never flip a flag off here.
    if let Some(color) = &style.color {
        node.style_props.color = Some(color.clone());
        node.style_props.color_theme = None;
        node.rpr_authored.color = true;
        node.rpr_authored.color_theme = false;
    }
    if let Some(highlight) = &style.highlight {
        node.style_props.highlight = Some(highlight.clone());
        node.rpr_authored.highlight = true;
    }
    if let Some(font_family) = &style.font_family {
        node.style_props.font_family = Some(font_family.clone());
        node.style_props.font_family_theme = None;
        // Imported runs may carry an exact source-form rFonts child because
        // independently-authored script slots are layout data. A font edit is
        // new authorship: retaining that old child would override the requested
        // family during serialization. The v1 edit contract authors the Latin
        // family in both ascii and hAnsi, so let the typed emitter synthesize
        // that documented shape.
        node.style_props.preserved.retain(|prop| {
            prop.name
                .rsplit_once(':')
                .map_or(prop.name.as_str(), |(_, local)| local)
                != "rFonts"
        });
        node.rpr_authored.font_family = true;
        node.rpr_authored.font_family_theme = false;
    }
    if let Some(size) = style.font_size_half_points {
        node.style_props.font_size = Some(size);
        node.rpr_authored.font_size = true;
    }
    // Character spacing (w:spacing @w:val, §17.3.2.35), twips. `0` is a valid
    // explicit value, so we honor `Some(0)` rather than treating it as unset.
    if let Some(spacing) = style.char_spacing {
        node.style_props.char_spacing = Some(spacing);
        node.rpr_authored.char_spacing = true;
    }

    // No-op if THIS operation changed nothing vs the live state — leave any
    // existing tracked formatting change untouched. Provenance counts: pinning an
    // inherited value as direct (an `rpr_authored` slot flips) is a real change
    // even when marks/style_props are unchanged.
    let current_directness = node.rpr_authored;
    if node.marks == live_marks
        && node.style_props == live_style_props
        && current_directness == previous_directness
    {
        return false;
    }

    match mode {
        // Author a tracked change: record the ORIGINAL rPr (baseline) so the
        // serializer emits one w:rPrChange whose reject-all restores the run's
        // pre-any-format state, even after several stacked format edits.
        MaterializationMode::TrackedChange => {
            if node.formatting_change.is_none() {
                node.formatting_change = Some(FormattingChange {
                    revision_id: revision.revision_id,
                    identity: 0,
                    previous_marks: baseline_marks,
                    previous_style_props: baseline_style_props,
                    previous_rpr_authored: baseline_rpr_authored,
                    author: revision.author.clone().unwrap_or_default(),
                    date: revision.date.clone(),
                });
            }
        }
        // Direct mutation: keep the new marks, no tracked change.
        MaterializationMode::Direct => {
            node.formatting_change = None;
        }
    }
    true
}

/// Summarize the formatting a span already carries, for the no-op refusal.
///
/// Marks first (they are what most patches ask for), then the style slots a
/// patch can set. Runs are summarized as a set: a span whose runs disagree
/// reports every state present, because "some bold" is exactly the situation a
/// caller needs to see.
fn describe_run_formatting(inlines: &[InlineNode]) -> String {
    let mut marks: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut colors: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut sizes: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    let mut any_text = false;
    for inline in inlines {
        let InlineNode::Text(text) = inline else {
            continue;
        };
        any_text = true;
        for mark in &text.marks {
            marks.insert(format!("{mark:?}").to_lowercase());
        }
        if let Some(color) = &text.style_props.color {
            colors.insert(color.to_string());
        }
        if let Some(size) = text.style_props.font_size {
            sizes.insert(size);
        }
    }
    if !any_text {
        return "no text runs".to_string();
    }
    let mut parts = Vec::new();
    parts.push(if marks.is_empty() {
        "marks=none".to_string()
    } else {
        format!("marks={}", marks.into_iter().collect::<Vec<_>>().join("+"))
    });
    if !colors.is_empty() {
        parts.push(format!(
            "color={}",
            colors.into_iter().collect::<Vec<_>>().join("/")
        ));
    }
    if !sizes.is_empty() {
        parts.push(format!(
            "size={}",
            sizes
                .into_iter()
                .map(|size| size.to_string())
                .collect::<Vec<_>>()
                .join("/")
        ));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::super::super::{InlineMarkSet, RunStyleEdit};
    use super::is_valid_color;

    #[test]
    fn caps_and_small_caps_count_as_non_empty_marks() {
        // The widened mark set must not report empty when only a tri-state
        // style mark (caps / smallCaps) is requested — otherwise the verb would
        // reject a legitimate "small-caps this term" as a no-op.
        let caps_only = InlineMarkSet {
            caps: true,
            ..Default::default()
        };
        assert!(!caps_only.is_empty());

        let small_caps_only = InlineMarkSet {
            small_caps: true,
            ..Default::default()
        };
        assert!(!small_caps_only.is_empty());

        assert!(InlineMarkSet::default().is_empty());
    }

    #[test]
    fn char_spacing_counts_as_non_empty_style() {
        // `Some(0)` is a legitimate explicit value (reset tracking), so it must
        // register as a real request, not be conflated with `None`.
        let zero = RunStyleEdit {
            char_spacing: Some(0),
            ..Default::default()
        };
        assert!(!zero.is_empty());

        let expand = RunStyleEdit {
            char_spacing: Some(40),
            ..Default::default()
        };
        assert!(!expand.is_empty());

        assert!(RunStyleEdit::default().is_empty());
    }

    #[test]
    fn color_accepts_six_hex_and_auto() {
        assert!(is_valid_color("FF0000"));
        assert!(is_valid_color("00aa99"));
        assert!(is_valid_color("auto"));
    }

    #[test]
    fn color_rejects_malformed_values() {
        // No silent coercion: reject `#` prefix, shorthand, named colors, bad len.
        assert!(!is_valid_color("#FF0000"));
        assert!(!is_valid_color("F00"));
        assert!(!is_valid_color("red"));
        assert!(!is_valid_color("FF00000"));
        assert!(!is_valid_color("GGGGGG"));
        assert!(!is_valid_color(""));
    }
}
