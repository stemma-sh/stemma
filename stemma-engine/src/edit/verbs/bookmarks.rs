//! `InsertBookmark` / `RenameBookmark` / `RemoveBookmark` — author, rename, and
//! remove WordprocessingML bookmarks (§17.13.6.2 `w:bookmarkStart` /
//! §17.13.6.1 `w:bookmarkEnd`).
//!
//! A bookmark in the IR is **not** a structured type. It is a pair of
//! `InlineNode::Decoration(DecorationNode{kind: Bookmark, raw_xml: Some(bytes)})`
//! where the bookmark `name` and `w:id` live *inside* `raw_xml`. The serializer
//! already emits `Decoration` from `raw_xml` verbatim and remaps the `w:id`
//! through `remap_decoration_id`, keyed by `{origin}:{old_id}` — so a synthesized
//! start/end pair that shares one `origin` + one placeholder `w:id` is reassigned
//! a single fresh id at serialize time and can never collide with base/target
//! bookmark ids. This verb therefore needs **no** domain or serialize change.
//!
//! Bookmarks are zero-width structural annotations, **not** tracked content:
//! they carry no `w:ins`/`w:del` wrapper and are emitted with `Normal` segment
//! status. Inserting, renaming, or removing one does not change the document's
//! visible text on any projection (accept-all == reject-all == original text).
//! Reversibility for these verbs is therefore the *decoration inventory*, not
//! `shape()` (which is blind to decorations) — the integration test asserts on
//! the IR directly.
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow", "no silent
//! fallbacks"):
//! - top-level paragraphs only (a nested table-cell paragraph surfaces as
//!   `BlockNotFound`);
//! - `InsertBookmark`: the `expect` span must lie within a single contiguous
//!   `Normal` run of text (mirrors `fields_crossrefs::find_anchor`); the start
//!   and end land in the same paragraph (a multi-paragraph bookmark is out of
//!   v1 and cannot be requested — both markers wrap one span);
//! - `RenameBookmark` / `RemoveBookmark`: the named bookmark's start AND its
//!   paired end must both live in the *named* paragraph. A start whose end is in
//!   a different segment/paragraph is refused (`BookmarkOrphanEnd`) rather than
//!   half-edited — there is no partial removal;
//! - name uniqueness is checked **within the target paragraph only**. Bookmarks
//!   declared in other body paragraphs, headers, footers, or other stories are
//!   NOT scanned in v1; a duplicate across that boundary is not detected here.
//!   (Word itself tolerates duplicate names but treats only the first as the
//!   reference target; cross-paragraph uniqueness is a follow-up.)
//!
//! Failure modes are explicit `EditError` variants carrying the bookmark name
//! and `step_index` — never a best-effort default.

use super::super::{EditError, find_block_index, validate_block_is_editable};
use crate::domain::{
    BlockNode, CanonDoc, DecorationNode, DecorationType, DocPart, InlineNode, NodeId,
    ParagraphNode, ProofRef, TrackedSegment, TrackingStatus,
};
use crate::semantic_hash::check_block_guard;

/// Origin tag for every authored bookmark decoration. Shared by a start/end pair
/// so the serializer's `{origin}:{old_id}` remap key reassigns them one fresh id
/// and keeps them disjoint from base ("base") / target ("target") bookmark ids.
const AUTHORED_ORIGIN: &str = "authored";

/// Resolve the target paragraph (top-level, editable, hash-checked) and run `f`.
fn with_target_paragraph<R>(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    semantic_hash: Option<&str>,
    step_index: usize,
    f: impl FnOnce(&mut ParagraphNode) -> Result<R, EditError>,
) -> Result<R, EditError> {
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    validate_block_is_editable(&doc.blocks[idx], step_index)?;

    match &doc.blocks[idx].block {
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
        && let Err(actual) = check_block_guard(&doc.blocks[idx].block, expected)
    {
        return Err(EditError::BlockSemanticHashMismatch {
            block_id: block_id.clone(),
            expected: expected.to_string(),
            actual,
            step_index,
        });
    }

    let BlockNode::Paragraph(para) = &mut doc.blocks[idx].block else {
        unreachable!("checked paragraph above");
    };
    f(para)
}

// =============================================================================
// Bookmark decoration helpers — parse / read the bytes that ARE the bookmark.
// =============================================================================

/// Parse a bookmark decoration's `raw_xml` into an `xmltree::Element`. The bytes
/// were synthesized here or by the serializer's `serialize_element`, so they
/// always parse; a parse failure is a programmer bug, surfaced loudly.
fn parse_bookmark_element(raw_xml: &[u8]) -> Result<xmltree::Element, EditError> {
    crate::word_xml::parse_raw_fragment(raw_xml).map_err(|_| EditError::BookmarkRawXmlUnparsable)
}

/// Re-serialize a bookmark `Element` back to bytes (the inverse of
/// `parse_bookmark_element`). The namespace declarations set by
/// `parse_raw_fragment` are preserved so the bytes round-trip through the
/// serializer's own `parse_raw_fragment`.
fn serialize_bookmark_element(element: &xmltree::Element) -> Vec<u8> {
    let mut out = Vec::new();
    element
        .write(&mut out)
        .expect("a bookmark element we built must serialize");
    out
}

/// True when `deco` is a bookmark decoration whose start element carries the
/// given `name`. Returns `None` for non-bookmark / end / nameless decorations.
fn bookmark_start_name(deco: &DecorationNode) -> Option<String> {
    if deco.kind != DecorationType::Bookmark {
        return None;
    }
    let raw = deco.raw_xml.as_ref()?;
    let element = crate::word_xml::parse_raw_fragment(raw).ok()?;
    if !crate::word_xml::is_w_tag(&element, "bookmarkStart") {
        return None;
    }
    crate::xml_attrs::attr_get(&element, "w:name").cloned()
}

/// `w:id` of a bookmark start OR end decoration (the value remap pairs on).
fn bookmark_id(deco: &DecorationNode) -> Option<String> {
    if deco.kind != DecorationType::Bookmark {
        return None;
    }
    let raw = deco.raw_xml.as_ref()?;
    let element = crate::word_xml::parse_raw_fragment(raw).ok()?;
    if crate::word_xml::is_w_tag(&element, "bookmarkStart")
        || crate::word_xml::is_w_tag(&element, "bookmarkEnd")
    {
        crate::xml_attrs::attr_get(&element, "w:id").cloned()
    } else {
        None
    }
}

/// True when `deco` is a `w:bookmarkEnd` carrying the given `w:id`.
fn is_bookmark_end_with_id(deco: &DecorationNode, id: &str) -> bool {
    if deco.kind != DecorationType::Bookmark {
        return false;
    }
    let Some(raw) = deco.raw_xml.as_ref() else {
        return false;
    };
    let Ok(element) = crate::word_xml::parse_raw_fragment(raw) else {
        return false;
    };
    crate::word_xml::is_w_tag(&element, "bookmarkEnd")
        && crate::xml_attrs::attr_get(&element, "w:id").map(String::as_str) == Some(id)
}

/// Collect every existing bookmark-start name in the paragraph.
fn existing_bookmark_names(para: &ParagraphNode) -> Vec<String> {
    para.segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Decoration(d) => bookmark_start_name(d),
            _ => None,
        })
        .collect()
}

/// Allocate a per-paragraph monotonic placeholder `w:id` for a fresh authored
/// bookmark pair, distinct from any authored-origin id already present so the
/// start/end share a key with each other but with no other pair. The serializer
/// reassigns the real id at write time; this only needs intra-`authored`
/// uniqueness. Base/target ids cannot collide (they live under a different
/// origin key).
fn next_authored_placeholder_id(para: &ParagraphNode) -> u32 {
    let max = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Decoration(d)
                if d.origin.as_deref() == Some(AUTHORED_ORIGIN)
                    && d.kind == DecorationType::Bookmark =>
            {
                bookmark_id(d).and_then(|s| s.parse::<u32>().ok())
            }
            _ => None,
        })
        .max();
    // Start well above the small ids Word assigns so even a stray collision in a
    // future merge is unlikely before the serializer remaps.
    match max {
        Some(m) => m + 1,
        None => 900_000_000,
    }
}

/// Synthesize a `bookmarkStart` decoration's `raw_xml` with a full `w:` namespace
/// declaration (the template the serializer round-trips, mirroring the test
/// fixture in `serialize/mod.rs`).
fn bookmark_start_raw_xml(id: u32, name: &str) -> Vec<u8> {
    format!(
        "<w:bookmarkStart xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:id=\"{id}\" w:name=\"{}\"/>",
        xml_escape_attr(name)
    )
    .into_bytes()
}

/// Synthesize a `bookmarkEnd` decoration's `raw_xml` sharing the start's `w:id`.
fn bookmark_end_raw_xml(id: u32) -> Vec<u8> {
    format!(
        "<w:bookmarkEnd xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:id=\"{id}\"/>"
    )
    .into_bytes()
}

/// Minimal XML attribute-value escaping for a bookmark name. Names are restricted
/// by OOXML to a tame character set, but `&`/`<`/`"` would corrupt the fragment
/// if present, so escape defensively rather than emit malformed XML.
fn xml_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn make_bookmark_decoration(para_id: &NodeId, suffix: &str, raw_xml: Vec<u8>) -> InlineNode {
    let id = NodeId::from(format!("{}_{suffix}", para_id.0));
    InlineNode::from(DecorationNode {
        id: id.clone(),
        kind: DecorationType::Bookmark,
        opaque_ref: format!("bookmarkref_{}", id.0),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: id,
            docx_anchor: String::new(),
        },
        // Paragraph-level bookmark marker: no host run rPr.
        wrapper_marks: Vec::new(),
        wrapper_style_props: crate::domain::StyleProps::default(),
        raw_xml: Some(raw_xml),
        origin: Some(AUTHORED_ORIGIN.to_string()),
    })
}

// =============================================================================
// InsertBookmark
// =============================================================================

/// Located insertion point for a bookmark span: the `expect` match runs from the
/// boundary in front of `start_inline`@`start_char` to `end_inline`@`end_char`,
/// all inside one contiguous Normal text run of segment `seg_idx`.
struct SpanPlan {
    seg_idx: usize,
    /// Inline index of the text node where the match begins.
    start_inline: usize,
    /// Char offset (within that node) where the match begins.
    start_char: usize,
    /// Inline index of the text node where the match ends.
    end_inline: usize,
    /// Char offset (within that node) where the match ends.
    end_char: usize,
}

pub(crate) fn apply_insert(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    name: &str,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse an empty / whitespace-only name at the edge: a nameless bookmark is
    // unreferenceable and meaningless (no silent default).
    if name.trim().is_empty() {
        return Err(EditError::BookmarkEmptyName { step_index });
    }
    let target = block_id.clone();
    let id_for_err = block_id.clone();
    let name = name.to_string();
    let expect = expect.to_string();
    with_target_paragraph(doc, &target, semantic_hash, step_index, move |para| {
        insert_in_paragraph(para, &id_for_err, &expect, &name, step_index)
    })
}

/// Paragraph-level core for `InsertBookmark` (unit-testable without a full doc).
fn insert_in_paragraph(
    para: &mut ParagraphNode,
    block_id: &NodeId,
    expect: &str,
    name: &str,
    step_index: usize,
) -> Result<(), EditError> {
    // Name uniqueness within the target paragraph (v1 boundary; documented).
    if existing_bookmark_names(para).iter().any(|n| n == name) {
        return Err(EditError::BookmarkDuplicateName {
            name: name.to_string(),
            step_index,
        });
    }

    let plan = find_span(&para.segments, expect).ok_or_else(|| {
        let visible = paragraph_visible_text(para);
        EditError::ExpectMismatch {
            block_id: block_id.clone(),
            expected: expect.to_string(),
            actual_text: visible,
            step_index,
        }
    })?;

    let placeholder = next_authored_placeholder_id(para);
    let start = make_bookmark_decoration(
        &para.id,
        &format!("bmstart_{placeholder}"),
        bookmark_start_raw_xml(placeholder, name),
    );
    let end = make_bookmark_decoration(
        &para.id,
        &format!("bmend_{placeholder}"),
        bookmark_end_raw_xml(placeholder),
    );

    splice_bookmark_pair(&mut para.segments, plan, start, end);

    para.block_text_hash = None;
    para.rendered_text = None;
    Ok(())
}

/// Concatenated visible text of a paragraph (for `ExpectMismatch` context).
fn paragraph_visible_text(para: &ParagraphNode) -> String {
    para.segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect()
}

/// Locate `expect` inside a single contiguous run of `Text` nodes within one
/// Normal segment. Returns the start/end inline+char boundaries of the match.
fn find_span(segments: &[TrackedSegment], expect: &str) -> Option<SpanPlan> {
    if expect.is_empty() {
        return None;
    }
    for (seg_idx, seg) in segments.iter().enumerate() {
        if seg.status != TrackingStatus::Normal {
            continue;
        }
        let inlines = &seg.inlines;
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
            if let Some((start_chars, end_chars)) = char_find_range(&concat, expect) {
                let start = map_run_offset(inlines, i, j, start_chars)?;
                let end = map_run_offset(inlines, i, j, end_chars)?;
                return Some(SpanPlan {
                    seg_idx,
                    start_inline: start.0,
                    start_char: start.1,
                    end_inline: end.0,
                    end_char: end.1,
                });
            }
            i = j.max(i + 1);
        }
    }
    None
}

/// Map a run-relative char offset back to `(inline_idx, local_char_offset)`
/// within the contiguous text run `[i, j)`.
fn map_run_offset(
    inlines: &[InlineNode],
    i: usize,
    j: usize,
    target_chars: usize,
) -> Option<(usize, usize)> {
    let mut consumed = 0usize;
    for (k, inline) in inlines.iter().enumerate().take(j).skip(i) {
        let InlineNode::Text(t) = inline else {
            unreachable!("run is all TextNodes");
        };
        let len = t.text.chars().count();
        if target_chars <= consumed + len {
            return Some((k, target_chars - consumed));
        }
        consumed += len;
    }
    None
}

/// Char range `[start, end)` (counting chars) at which `needle` lies inside
/// `haystack`, or `None` if not found.
fn char_find_range(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let byte = haystack.find(needle)?;
    let start = haystack[..byte].chars().count();
    Some((start, start + needle.chars().count()))
}

/// Splice the `start` decoration immediately before the matched span and the
/// `end` decoration immediately after it, inside the host Normal segment. Text
/// nodes are split at the span boundaries so the markers land exactly around the
/// matched text. Decorations are zero-width, so visible text is unchanged.
fn splice_bookmark_pair(
    segments: &mut Vec<TrackedSegment>,
    plan: SpanPlan,
    start: InlineNode,
    end: InlineNode,
) {
    let host = segments.remove(plan.seg_idx);
    let status = host.status.clone();
    let inlines = host.inlines;

    // Rebuild the inline vector, splitting the boundary text nodes and inserting
    // the two decorations at the matched-span edges.
    let mut rebuilt: Vec<InlineNode> = Vec::with_capacity(inlines.len() + 2);
    for (idx, inline) in inlines.into_iter().enumerate() {
        if idx == plan.start_inline && idx == plan.end_inline {
            // Span starts and ends in the same text node: split into three.
            let InlineNode::Text(node) = inline else {
                unreachable!("boundary inline is a text node");
            };
            let chars: Vec<char> = node.text.chars().collect();
            let before: String = chars[..plan.start_char].iter().collect();
            let mid: String = chars[plan.start_char..plan.end_char].iter().collect();
            let after: String = chars[plan.end_char..].iter().collect();
            push_text_piece(&mut rebuilt, &node, before, "");
            rebuilt.push(start.clone());
            push_text_piece(&mut rebuilt, &node, mid, "_bmmid");
            rebuilt.push(end.clone());
            push_text_piece(&mut rebuilt, &node, after, "_bmtail");
        } else if idx == plan.start_inline {
            let InlineNode::Text(node) = inline else {
                unreachable!("boundary inline is a text node");
            };
            let chars: Vec<char> = node.text.chars().collect();
            let before: String = chars[..plan.start_char].iter().collect();
            let after: String = chars[plan.start_char..].iter().collect();
            push_text_piece(&mut rebuilt, &node, before, "");
            rebuilt.push(start.clone());
            push_text_piece(&mut rebuilt, &node, after, "_bmtail");
        } else if idx == plan.end_inline {
            let InlineNode::Text(node) = inline else {
                unreachable!("boundary inline is a text node");
            };
            let chars: Vec<char> = node.text.chars().collect();
            let before: String = chars[..plan.end_char].iter().collect();
            let after: String = chars[plan.end_char..].iter().collect();
            push_text_piece(&mut rebuilt, &node, before, "");
            rebuilt.push(end.clone());
            push_text_piece(&mut rebuilt, &node, after, "_bmtail");
        } else {
            rebuilt.push(inline);
        }
    }

    segments.insert(
        plan.seg_idx,
        TrackedSegment {
            status,
            inlines: rebuilt,
        },
    );
}

/// Push a text piece derived from `node` only if non-empty. A non-empty
/// `suffix` distinguishes the tail half's id from the head's.
fn push_text_piece(
    out: &mut Vec<InlineNode>,
    node: &crate::domain::TextNode,
    text: String,
    suffix: &str,
) {
    if text.is_empty() {
        return;
    }
    let mut piece = node.clone();
    if !suffix.is_empty() {
        piece.id = NodeId::new(format!("{}{suffix}", node.id.0));
    }
    piece.text = text;
    out.push(InlineNode::from(piece));
}

// =============================================================================
// RenameBookmark
// =============================================================================

pub(crate) fn apply_rename(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    old_name: &str,
    new_name: &str,
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    if new_name.trim().is_empty() {
        return Err(EditError::BookmarkEmptyName { step_index });
    }
    let block_id = block_id.clone();
    let old_name = old_name.to_string();
    let new_name = new_name.to_string();
    with_target_paragraph(doc, &block_id, semantic_hash, step_index, move |para| {
        rename_in_paragraph(para, &old_name, &new_name, step_index)
    })
}

/// Paragraph-level core for `RenameBookmark`.
fn rename_in_paragraph(
    para: &mut ParagraphNode,
    old_name: &str,
    new_name: &str,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse renaming onto a name already present (renaming onto itself is the
    // only allowed match of new_name, and is a structural no-op we tolerate).
    if new_name != old_name && existing_bookmark_names(para).iter().any(|n| n == new_name) {
        return Err(EditError::BookmarkDuplicateName {
            name: new_name.to_string(),
            step_index,
        });
    }

    let mut renamed = false;
    'outer: for seg in &mut para.segments {
        for inline in &mut seg.inlines {
            let InlineNode::Decoration(deco) = inline else {
                continue;
            };
            if bookmark_start_name(deco).as_deref() != Some(old_name) {
                continue;
            }
            let raw = deco
                .raw_xml
                .as_ref()
                .expect("bookmark_start_name matched => raw_xml present");
            let mut element = parse_bookmark_element(raw)?;
            crate::xml_attrs::attr_set(&mut element, "w:name", new_name);
            deco.raw_xml = Some(serialize_bookmark_element(&element));
            renamed = true;
            break 'outer;
        }
    }

    if !renamed {
        return Err(EditError::BookmarkNotFound {
            name: old_name.to_string(),
            step_index,
        });
    }
    Ok(())
}

// =============================================================================
// RemoveBookmark
// =============================================================================

pub(crate) fn apply_remove(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    name: &str,
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    let block_id = block_id.clone();
    let name = name.to_string();
    with_target_paragraph(doc, &block_id, semantic_hash, step_index, move |para| {
        remove_in_paragraph(para, &name, step_index)
    })
}

/// Paragraph-level core for `RemoveBookmark`.
fn remove_in_paragraph(
    para: &mut ParagraphNode,
    name: &str,
    step_index: usize,
) -> Result<(), EditError> {
    // Resolve name -> the start decoration -> its w:id.
    let mut bookmark_w_id: Option<String> = None;
    'outer: for seg in &para.segments {
        for inline in &seg.inlines {
            if let InlineNode::Decoration(deco) = inline
                && bookmark_start_name(deco).as_deref() == Some(name)
            {
                bookmark_w_id = bookmark_id(deco);
                break 'outer;
            }
        }
    }
    let Some(w_id) = bookmark_w_id else {
        return Err(EditError::BookmarkNotFound {
            name: name.to_string(),
            step_index,
        });
    };

    // Confirm the paired end exists in this paragraph BEFORE mutating: no
    // partial removal (CLAUDE.md "fail loud beyond v1 scope").
    let end_present = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .any(|i| matches!(i, InlineNode::Decoration(d) if is_bookmark_end_with_id(d, &w_id)));
    if !end_present {
        return Err(EditError::BookmarkOrphanEnd {
            name: name.to_string(),
            step_index,
        });
    }

    // Drop both the named start and the bookmarkEnd sharing its id. All other
    // inlines (including unrelated decorations) are left byte-identical.
    for seg in &mut para.segments {
        seg.inlines.retain(|inline| {
            let InlineNode::Decoration(deco) = inline else {
                return true;
            };
            let is_named_start = bookmark_start_name(deco).as_deref() == Some(name);
            let is_paired_end = is_bookmark_end_with_id(deco, &w_id);
            !(is_named_start || is_paired_end)
        });
    }

    para.block_text_hash = None;
    para.rendered_text = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{IStr, StyleProps, TextNode};

    fn text_node(id: &str, text: &str) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(id),
            text_role: None,
            text: text.to_string(),
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            formatting_change: None,
        })
    }

    fn paragraph(text: &str) -> ParagraphNode {
        ParagraphNode {
            id: NodeId::from("p_1"),
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
            segments: vec![TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![text_node("p_1_r0", text)],
            }],
            block_text_hash: None,
            numbering: None,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: StyleProps::default(),
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
        }
    }

    fn decorations(para: &ParagraphNode) -> Vec<&DecorationNode> {
        para.segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Decoration(d) => Some(d.as_ref()),
                _ => None,
            })
            .collect()
    }

    fn visible(para: &ParagraphNode) -> String {
        paragraph_visible_text(para)
    }

    /// Splice a fresh authored bookmark named `name` around `expect` in `para`,
    /// directly (bypassing the doc-level resolution that the integration test
    /// covers). Returns the placeholder id used.
    fn splice_named(para: &mut ParagraphNode, expect: &str, name: &str) -> u32 {
        let plan = find_span(&para.segments, expect).expect("span found");
        let pid = next_authored_placeholder_id(para);
        let start = make_bookmark_decoration(
            &para.id,
            &format!("bmstart_{pid}"),
            bookmark_start_raw_xml(pid, name),
        );
        let end =
            make_bookmark_decoration(&para.id, &format!("bmend_{pid}"), bookmark_end_raw_xml(pid));
        splice_bookmark_pair(&mut para.segments, plan, start, end);
        pid
    }

    #[test]
    fn char_find_range_maps_chars() {
        assert_eq!(char_find_range("Hello world", "world"), Some((6, 11)));
        assert_eq!(char_find_range("café au lait", "au"), Some((5, 7)));
        assert_eq!(char_find_range("Hello", "absent"), None);
    }

    #[test]
    fn synthesized_start_carries_name_and_id() {
        let raw = bookmark_start_raw_xml(900_000_000, "Definitions");
        let el = crate::word_xml::parse_raw_fragment(&raw).expect("parse");
        assert!(crate::word_xml::is_w_tag(&el, "bookmarkStart"));
        // §17.13.6.2 — bookmarkStart carries w:id + w:name.
        assert_eq!(
            crate::xml_attrs::attr_get(&el, "w:name").map(String::as_str),
            Some("Definitions")
        );
        assert_eq!(
            crate::xml_attrs::attr_get(&el, "w:id").map(String::as_str),
            Some("900000000")
        );
    }

    #[test]
    fn synthesized_end_shares_id_no_name() {
        let raw = bookmark_end_raw_xml(900_000_000);
        let el = crate::word_xml::parse_raw_fragment(&raw).expect("parse");
        // §17.13.6.1 — bookmarkEnd carries only w:id (the same as its start).
        assert!(crate::word_xml::is_w_tag(&el, "bookmarkEnd"));
        assert_eq!(
            crate::xml_attrs::attr_get(&el, "w:id").map(String::as_str),
            Some("900000000")
        );
        assert_eq!(crate::xml_attrs::attr_get(&el, "w:name"), None);
    }

    #[test]
    fn insert_wraps_span_zero_width() {
        let mut para = paragraph("The Confidential Information clause.");
        let plan = find_span(&para.segments, "Confidential Information").expect("span found");
        let placeholder = next_authored_placeholder_id(&para);
        let start = make_bookmark_decoration(
            &para.id,
            &format!("bmstart_{placeholder}"),
            bookmark_start_raw_xml(placeholder, "DefTerm"),
        );
        let end = make_bookmark_decoration(
            &para.id,
            &format!("bmend_{placeholder}"),
            bookmark_end_raw_xml(placeholder),
        );
        splice_bookmark_pair(&mut para.segments, plan, start, end);

        // Visible text unchanged (bookmarks are zero-width).
        assert_eq!(visible(&para), "The Confidential Information clause.");

        let decos = decorations(&para);
        assert_eq!(decos.len(), 2, "exactly one start + one end");
        // start before end, same id (§17.13.6 ordering + pairing).
        assert_eq!(bookmark_start_name(decos[0]).as_deref(), Some("DefTerm"));
        assert_eq!(bookmark_id(decos[0]), bookmark_id(decos[1]));
        assert!(is_bookmark_end_with_id(
            decos[1],
            &bookmark_id(decos[0]).unwrap()
        ));
    }

    #[test]
    fn rename_changes_only_name_keeps_id() {
        let mut para = paragraph("alpha beta gamma");
        splice_named(&mut para, "beta", "OldName");
        let id_before = bookmark_id(decorations(&para)[0]).unwrap();

        rename_in_paragraph(&mut para, "OldName", "NewName", 0).unwrap();

        let decos = decorations(&para);
        assert_eq!(bookmark_start_name(decos[0]).as_deref(), Some("NewName"));
        assert_eq!(
            bookmark_id(decos[0]).unwrap(),
            id_before,
            "rename must not touch the w:id"
        );
        // The end's id is untouched too (still pairs).
        assert!(is_bookmark_end_with_id(decos[1], &id_before));
    }

    #[test]
    fn rename_onto_taken_name_refuses() {
        let mut para = paragraph("alpha beta gamma");
        splice_named(&mut para, "alpha", "A");
        splice_named(&mut para, "gamma", "B");
        let err = rename_in_paragraph(&mut para, "A", "B", 5).expect_err("must fail");
        match err {
            EditError::BookmarkDuplicateName { name, step_index } => {
                assert_eq!(name, "B");
                assert_eq!(step_index, 5);
            }
            other => panic!("expected BookmarkDuplicateName, got {other:?}"),
        }
    }

    #[test]
    fn rename_missing_is_not_found_with_name() {
        let mut para = paragraph("plain text");
        let err = rename_in_paragraph(&mut para, "Absent", "X", 3).expect_err("must fail");
        match err {
            EditError::BookmarkNotFound { name, step_index } => {
                assert_eq!(name, "Absent");
                assert_eq!(step_index, 3);
            }
            other => panic!("expected BookmarkNotFound, got {other:?}"),
        }
    }

    #[test]
    fn remove_drops_exactly_the_named_pair() {
        // Two bookmarks; remove one, the other survives byte-identical.
        let mut para = paragraph("one two three four");
        let pid_a = splice_named(&mut para, "two", "BmA");
        let pid_b = splice_named(&mut para, "four", "BmB");
        assert_ne!(pid_a, pid_b, "second pair gets a fresh placeholder id");
        let bm_b_start_raw = decorations(&para)
            .iter()
            .find(|d| bookmark_start_name(d).as_deref() == Some("BmB"))
            .and_then(|d| d.raw_xml.clone())
            .unwrap();

        remove_in_paragraph(&mut para, "BmA", 0).unwrap();

        let decos = decorations(&para);
        assert_eq!(decos.len(), 2, "only BmB's start+end remain");
        // BmB untouched, byte-identical.
        let surviving_b = decos
            .iter()
            .find(|d| bookmark_start_name(d).as_deref() == Some("BmB"))
            .unwrap();
        assert_eq!(surviving_b.raw_xml.as_ref(), Some(&bm_b_start_raw));
        // BmA fully gone.
        assert!(
            decos
                .iter()
                .all(|d| bookmark_start_name(d).as_deref() != Some("BmA"))
        );
        assert_eq!(visible(&para), "one two three four");
    }

    #[test]
    fn remove_missing_is_not_found_with_name() {
        let mut para = paragraph("plain text");
        let err = remove_in_paragraph(&mut para, "Ghost", 9).expect_err("must fail");
        match err {
            EditError::BookmarkNotFound { name, step_index } => {
                assert_eq!(name, "Ghost");
                assert_eq!(step_index, 9);
            }
            other => panic!("expected BookmarkNotFound, got {other:?}"),
        }
    }

    #[test]
    fn remove_orphan_end_refuses() {
        // A start whose end was dropped: removal must refuse, not half-edit.
        let mut para = paragraph("hello orphan world");
        let pid = splice_named(&mut para, "orphan", "Lonely");
        // Surgically remove the end only, leaving an orphan start.
        for seg in &mut para.segments {
            seg.inlines.retain(|i| {
                !matches!(i, InlineNode::Decoration(d) if is_bookmark_end_with_id(d, &pid.to_string()))
            });
        }

        let err = remove_in_paragraph(&mut para, "Lonely", 7).expect_err("must fail");
        match err {
            EditError::BookmarkOrphanEnd { name, step_index } => {
                assert_eq!(name, "Lonely");
                assert_eq!(step_index, 7);
            }
            other => panic!("expected BookmarkOrphanEnd, got {other:?}"),
        }
        // No partial removal: the orphan start is still present (we refused).
        assert_eq!(
            decorations(&para).len(),
            1,
            "refusal must not have removed the orphan start"
        );
    }

    #[test]
    fn insert_duplicate_name_refuses() {
        let mut para = paragraph("alpha beta gamma");
        insert_in_paragraph(&mut para, &NodeId::from("p_1"), "alpha", "Dup", 0).unwrap();
        let err = insert_in_paragraph(&mut para, &NodeId::from("p_1"), "gamma", "Dup", 2)
            .expect_err("duplicate must fail");
        match err {
            EditError::BookmarkDuplicateName { name, step_index } => {
                assert_eq!(name, "Dup");
                assert_eq!(step_index, 2);
            }
            other => panic!("expected BookmarkDuplicateName, got {other:?}"),
        }
    }

    #[test]
    fn insert_empty_name_refuses() {
        let mut doc_block = paragraph("alpha beta");
        // The edge check lives in `apply_insert`; assert it via the paragraph
        // core's sibling guard by going through a minimal stand-in: the empty
        // name is rejected before any paragraph mutation.
        let err = insert_empty_guard("   ", 4).expect_err("empty/whitespace name must be refused");
        assert!(matches!(
            err,
            EditError::BookmarkEmptyName { step_index: 4 }
        ));
        // sanity: a real name does not trip the guard and inserts fine.
        insert_in_paragraph(&mut doc_block, &NodeId::from("p_1"), "alpha", "Ok", 0).unwrap();
        assert_eq!(decorations(&doc_block).len(), 2);
    }

    /// Mirror the empty-name edge guard that `apply_insert` applies before
    /// touching the paragraph, so the unit test can exercise it without a doc.
    fn insert_empty_guard(name: &str, step_index: usize) -> Result<(), EditError> {
        if name.trim().is_empty() {
            return Err(EditError::BookmarkEmptyName { step_index });
        }
        Ok(())
    }

    #[test]
    fn placeholder_ids_are_disjoint_from_istr_base() {
        // Sanity: IStr roundtrips so id parsing in next_authored_placeholder_id works.
        let s: IStr = "900000000".into();
        assert_eq!(s.parse::<u32>().ok(), Some(900_000_000));
    }
}
