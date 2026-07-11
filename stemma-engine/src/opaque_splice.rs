//! The fragment tracked-splice core (RFC-0002 §Phase-1) — a surgical text
//! replacement inside one text region of an opaque fragment, producing OOXML
//! tracked-change markup (`w:del`/`w:ins` with author/date) or a direct
//! in-place replace. Everything outside the edited runs is preserved
//! structurally: the fragment is re-serialized through the same emitter that
//! produced the imported `raw_xml`, so untouched content is Word-identical
//! (untouched sibling runs may be re-emitted in normalized form — e.g. an
//! explicit `xml:space` — which Word renders identically; this is NOT a
//! byte-for-byte guarantee on sibling runs).
//!
//! This is the ONE helper both `opaque_text_edit` (textbox ¶ + inline SDT) and
//! `sdt_text_fill` (inline + body-level SDT) share. It operates purely on an
//! `xmltree::Element` region (a `w:p` inside a `w:txbxContent`, or a
//! `w:sdtContent`) so it is reachable from the edit verbs AND from the serialize
//! path that patches body-level block-SDT bytes in the scaffold.
//!
//! ## Semantics
//!
//! Replace the FIRST occurrence of `find` in the region's visible `w:t` text —
//! first in DOCUMENT ORDER over the region's editable text, direct runs and
//! transparent wrappers (hyperlink/smartTag) interleaved as written, matching
//! the text discovery reports. In tracked mode the deleted text becomes
//! `<w:del><w:r><w:delText>…` and the inserted text `<w:ins><w:r><w:t>…`, each
//! stamped with a fresh unique revision id (the whole-document minting
//! counter) plus the author/date. In direct mode the old runs are replaced by
//! one new run.
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - `find` not present in the region            → `TextNotFound`
//! - the region already carries `w:ins`/`w:del`   → `RegionHasTrackedChanges`
//! - the matched span crosses a non-text element  → `UnsupportedRegionShape`
//!   (a `w:tab`/`w:br`/`w:drawing`/nested control between the affected runs)
//!
//! No partial write ever escapes: the region is rebuilt only after the whole
//! splice is validated, so an error leaves the caller's fragment untouched.

use xmltree::{Element, XMLNode};

use crate::domain::RevisionInfo;
use crate::word_xml::w_el;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SpliceError {
    /// `find` does not occur in the region's visible text.
    TextNotFound,
    /// The region already contains tracked-change markup; resolve it first.
    RegionHasTrackedChanges,
    /// The matched span crosses a non-text element (tab/break/drawing/nested
    /// control) — out of v1 surgical-splice scope.
    UnsupportedRegionShape,
    /// A whole-value SET was asked to fill a region whose text is NOT entirely in
    /// direct simple runs — some lives inside a barrier (a hyperlink, field, or
    /// nested control). Setting the value would relocate/duplicate that barrier
    /// text (it is not deletable in place), so we refuse rather than corrupt it: a
    /// rich content region is not a cleanly fillable value.
    RegionHasComplexContent,
}

/// Splice `replacement` in for the first DOCUMENT-ORDER occurrence of `find`
/// in `region`'s editable text. `base` supplies author/date; `rev_counter`
/// mints the fresh unique ids for the `w:del`/`w:ins` (advanced by up to two).
///
/// Text inside a TRANSPARENT wrapper (a `w:hyperlink` / `w:smartTag`) is ordinary
/// editable text — Word lets you type in a hyperlink freely — so the occurrence
/// search interleaves wrapper text with the direct runs in document order, and
/// an occurrence inside a wrapper is spliced INSIDE it, keeping the wrapper
/// intact and placing the tracked `w:ins`/`w:del` within it (valid
/// `EG_PContent`; accept/reject already recurse through wrappers). A span that
/// straddles a container boundary is not present in any single container and
/// refuses (`TextNotFound`) — editing across a link boundary is genuinely
/// ambiguous.
pub(crate) fn splice_region_text(
    region: &mut Element,
    find: &str,
    replacement: &str,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
) -> Result<(), SpliceError> {
    if find.is_empty() {
        return Err(SpliceError::TextNotFound);
    }
    // A pre-existing tracked change anywhere in the editable text (direct runs
    // or inside a transparent wrapper) means the visible-text offsets below
    // would not match what a reviewer sees after resolution — refuse rather
    // than splice into an ambiguous base.
    if region_has_tracked_containers(region) {
        return Err(SpliceError::RegionHasTrackedChanges);
    }

    // "First occurrence" means first in the region's DOCUMENT-ORDER visible
    // text — the text discovery reports — with direct runs and transparent
    // wrappers interleaved as written. (Trying direct runs first would edit a
    // doc-LATER direct occurrence over a doc-earlier hyperlinked one.) Build
    // the doc-order segmentation, locate the occurrence, and splice inside
    // the ONE container that holds it; a span straddling a container
    // boundary is genuinely ambiguous and refuses.
    enum Seg {
        /// Text in this region's own direct runs; `direct_prefix` is the byte
        /// offset of this segment within the direct-runs-only concatenation.
        Direct { direct_prefix: usize },
        /// Text inside the transparent wrapper at child index `i`.
        Wrapper { child_index: usize },
    }
    let mut segments: Vec<(Seg, usize)> = Vec::new(); // (segment, byte length)
    let mut full = String::new();
    let mut direct_len = 0usize;
    // One Direct segment per maximal run of non-wrapper children: adjacent
    // direct runs are the SAME container (a span across them is an ordinary
    // multi-run splice), and a barrier between them stays inside the segment —
    // a span reaching across the barrier's position refuses inside
    // `splice_direct_at` (`UnsupportedRegionShape`). Only a transparent
    // wrapper closes a Direct segment, because its text interleaves.
    let mut open_direct: Option<(usize, usize)> = None; // (direct_prefix, byte len)
    for (i, child) in region.children.iter().enumerate() {
        if let XMLNode::Element(w) = child
            && is_transparent_wrapper(w)
        {
            if let Some((direct_prefix, len)) = open_direct.take() {
                segments.push((Seg::Direct { direct_prefix }, len));
            }
            let text = wrapper_visible_text(w);
            segments.push((Seg::Wrapper { child_index: i }, text.len()));
            full.push_str(&text);
        } else if let Some(text) = text_run_text(child) {
            match &mut open_direct {
                Some((_, len)) => *len += text.len(),
                None => open_direct = Some((direct_len, text.len())),
            }
            direct_len += text.len();
            full.push_str(&text);
        }
        // Other barrier children (drawings, fields, …) contribute no editable
        // text and do not close the open Direct segment.
    }
    if let Some((direct_prefix, len)) = open_direct.take() {
        segments.push((Seg::Direct { direct_prefix }, len));
    }

    let Some(start) = full.find(find) else {
        return Err(SpliceError::TextNotFound);
    };
    let end = start + find.len();

    // Locate the one container the occurrence lies in.
    let mut seg_start = 0usize;
    for (seg, len) in &segments {
        let seg_end = seg_start + len;
        // Skip zero-length and non-overlapping segments.
        if *len > 0 && start < seg_end && end > seg_start {
            if start < seg_start || end > seg_end {
                // Straddles this container's boundary — not present in any
                // single container's text: genuinely ambiguous, refuse.
                return Err(SpliceError::TextNotFound);
            }
            return match seg {
                Seg::Direct { direct_prefix } => splice_direct_at(
                    region,
                    find,
                    direct_prefix + (start - seg_start),
                    replacement,
                    base,
                    rev_counter,
                    tracked,
                ),
                Seg::Wrapper { child_index } => {
                    let XMLNode::Element(w) = &mut region.children[*child_index] else {
                        unreachable!("wrapper segment indexes a wrapper element");
                    };
                    splice_region_text(w, find, replacement, base, rev_counter, tracked)
                }
            };
        }
        seg_start = seg_end;
    }
    Err(SpliceError::TextNotFound)
}

/// Any tracked-change container in the region's editable text — its direct
/// children or (recursively) inside a transparent wrapper. Barriers are not
/// entered: their interiors are not editable text. This is the predicate the
/// splice refuses on (`RegionHasTrackedChanges`) and the one discovery uses to
/// mark a target `has_tracked_changes` — one predicate, so discovery and the
/// verb cannot disagree.
pub(crate) fn region_has_tracked_containers(region: &Element) -> bool {
    region.children.iter().any(|c| match c {
        XMLNode::Element(e) => {
            is_tracked_container(e)
                || (is_transparent_wrapper(e) && region_has_tracked_containers(e))
        }
        _ => false,
    })
}

/// The visible text of a transparent wrapper, document-order: its direct runs
/// interleaved with nested transparent wrappers. Barrier children contribute
/// nothing (matching the region-level segmentation).
fn wrapper_visible_text(wrapper: &Element) -> String {
    let mut out = String::new();
    for child in &wrapper.children {
        if let Some(text) = text_run_text(child) {
            out.push_str(&text);
        } else if let XMLNode::Element(e) = child
            && is_transparent_wrapper(e)
        {
            out.push_str(&wrapper_visible_text(e));
        }
    }
    out
}

/// A wrapper whose interior text Word treats as ordinary editable text — we
/// descend through it rather than refusing at it.
fn is_transparent_wrapper(e: &Element) -> bool {
    crate::word_xml::is_w_tag(e, "hyperlink") || crate::word_xml::is_w_tag(e, "smartTag")
}

/// Splice within this container's DIRECT runs at the given byte offset of the
/// direct-runs-only concatenation (the caller located the occurrence in
/// document order — see `splice_region_text` — and mapped it into direct
/// coordinates; this function must not re-search, or it could pick a different
/// occurrence).
fn splice_direct_at(
    region: &mut Element,
    find: &str,
    at_byte: usize,
    replacement: &str,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
) -> Result<(), SpliceError> {
    // Concatenate the visible text of the direct-child text runs, in order.
    let full: String = region.children.iter().filter_map(text_run_text).collect();
    let char_count = full.chars().count();

    // The caller's segmentation and this concatenation are the same direct-run
    // walk, so the located occurrence must sit exactly here — a mismatch is a
    // programmer bug in that agreement, not a document condition.
    if !full
        .get(at_byte..)
        .is_some_and(|tail| tail.starts_with(find))
    {
        debug_assert!(false, "doc-order occurrence must map into direct-run text");
        return Err(SpliceError::TextNotFound);
    }
    let byte_start = at_byte;
    // Convert byte offsets to char offsets (run text is split on char boundaries).
    let start = full[..byte_start].chars().count();
    let end = start + find.chars().count();

    // Rebuild the region's children, splitting the boundary runs and emitting the
    // del/ins block where the span sat. `offset` tracks the running char position
    // over text-run content ONLY (non-text children don't advance it).
    let mut new_children: Vec<XMLNode> = Vec::with_capacity(region.children.len() + 3);
    let mut offset = 0usize;
    let mut deleted: Vec<Element> = Vec::new();
    let mut insert_rpr: Option<Element> = None;
    let mut change_emitted = false;

    for child in &region.children {
        match text_run_parts(child) {
            None => {
                // A non-text child. If it sits strictly inside the matched span,
                // the span crosses it — unsupported. "Inside" = we have entered
                // the span (offset > start) but not finished it (offset < end).
                if offset > start && offset < end {
                    return Err(SpliceError::UnsupportedRegionShape);
                }
                new_children.push(child.clone());
            }
            Some((rpr, text)) => {
                let run_start = offset;
                let run_end = offset + text.chars().count();
                offset = run_end;

                let chars: Vec<char> = text.chars().collect();
                let pre_end = clamp(start, run_start, run_end) - run_start;
                let span_end = clamp(end, run_start, run_end) - run_start;
                let pre: String = chars[..pre_end].iter().collect();
                let span: String = chars[pre_end..span_end].iter().collect();
                let post: String = chars[span_end..].iter().collect();

                if !pre.is_empty() {
                    new_children.push(XMLNode::Element(text_run(rpr, &pre)));
                }
                if !span.is_empty() {
                    if insert_rpr.is_none() {
                        insert_rpr = Some(rpr.cloned().unwrap_or_else(|| w_el("rPr")));
                    }
                    deleted.push(deleted_run(rpr, &span));
                }
                // The run that contains the END of the span is where the change
                // block lands — after this run's pre, before its post.
                if !change_emitted && run_start < end && end <= run_end && end > start {
                    emit_change(
                        &mut new_children,
                        &mut deleted,
                        replacement,
                        insert_rpr.as_ref(),
                        base.author.as_deref().unwrap_or(""),
                        base.date.as_deref(),
                        &mut counter_mint(rev_counter),
                        tracked,
                    );
                    change_emitted = true;
                }
                if !post.is_empty() {
                    new_children.push(XMLNode::Element(text_run(rpr, &post)));
                }
            }
        }
    }

    // `find` was located in `full`, so the span must have been emitted. Guard the
    // invariant rather than trust it silently.
    debug_assert!(change_emitted, "located span must be spliced");
    if !change_emitted || end > char_count {
        return Err(SpliceError::TextNotFound);
    }

    region.children = new_children;
    Ok(())
}

/// Whole-value SET: replace ALL of a region's visible text with `value` (the
/// "set this content control's value" operation). In tracked mode every existing
/// text run is deleted (`w:del`) and `value` inserted (`w:ins`); in direct mode
/// the existing runs are dropped and one run carries `value`. Shares the run
/// builders with [`splice_region_text`]. Refuses a region that already carries
/// tracked changes; refuses a no-op (no existing text AND empty value).
pub(crate) fn set_region_text(
    region: &mut Element,
    value: &str,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
) -> Result<(), SpliceError> {
    set_region_text_inner(
        region,
        value,
        base.author.as_deref().unwrap_or(""),
        base.date.as_deref(),
        &mut counter_mint(rev_counter),
        tracked,
    )
}

/// Whole-value SET with EXPLICIT pre-minted revision ids (the save-time block-SDT
/// path, where the ids were minted at verb time from the transaction counter and
/// staged). `ids` supplies the `w:del` id then the `w:ins` id, in order.
pub(crate) fn set_region_text_with_ids(
    region: &mut Element,
    value: &str,
    author: &str,
    date: Option<&str>,
    ids: [u32; 2],
    tracked: bool,
) -> Result<(), SpliceError> {
    let mut it = ids.into_iter();
    set_region_text_inner(
        region,
        value,
        author,
        date,
        &mut || it.next().expect("at most two ids minted per set"),
        tracked,
    )
}

fn set_region_text_inner(
    region: &mut Element,
    value: &str,
    author: &str,
    date: Option<&str>,
    mint: &mut dyn FnMut() -> u32,
    tracked: bool,
) -> Result<(), SpliceError> {
    if region
        .children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(e) if is_tracked_container(e)))
    {
        return Err(SpliceError::RegionHasTrackedChanges);
    }

    // A whole-value SET deletes the region's DIRECT simple text runs and inserts
    // the value at their position. If any text lives inside a BARRIER child (a
    // hyperlink / field / nested control — not a simple run), it is not deletable
    // in place, so setting the value would leave it stranded and relocated. Refuse
    // rather than corrupt: such a region is not a cleanly fillable value.
    if region.children.iter().any(|c| match c {
        XMLNode::Element(e) => text_run_parts(c).is_none() && element_has_wt_text(e),
        _ => false,
    }) {
        return Err(SpliceError::RegionHasComplexContent);
    }

    let mut new_children: Vec<XMLNode> = Vec::with_capacity(region.children.len() + 2);
    let mut deleted: Vec<Element> = Vec::new();
    let mut insert_rpr: Option<Element> = None;
    let mut insert_at: Option<usize> = None;

    for child in &region.children {
        match text_run_parts(child) {
            None => new_children.push(child.clone()),
            Some((rpr, text)) => {
                if insert_at.is_none() {
                    insert_at = Some(new_children.len());
                }
                if insert_rpr.is_none() {
                    insert_rpr = Some(rpr.cloned().unwrap_or_else(|| w_el("rPr")));
                }
                if !text.is_empty() {
                    deleted.push(deleted_run(rpr, &text));
                }
                // The original text run is dropped — its content moves into the
                // deletion (tracked) or simply vanishes (direct).
            }
        }
    }

    if insert_at.is_none() && value.is_empty() {
        return Err(SpliceError::TextNotFound); // nothing to delete, nothing to add
    }
    let pos = insert_at.unwrap_or(new_children.len());
    let mut block = Vec::new();
    emit_change(
        &mut block,
        &mut deleted,
        value,
        insert_rpr.as_ref(),
        author,
        date,
        mint,
        tracked,
    );
    new_children.splice(pos..pos, block);
    region.children = new_children;
    Ok(())
}

/// Emit the tracked (`w:del` then `w:ins`) or direct replacement into `out`,
/// consuming the collected deleted runs. `mint` supplies the fresh `w:id` for
/// each emitted `w:ins`/`w:del` (a whole-document counter for the verb path, or a
/// pre-minted id list for the save-time block path), so the two paths stamp ids
/// the same way. `mint` is called ONLY when a tracked element is actually emitted.
#[allow(clippy::too_many_arguments)]
fn emit_change(
    out: &mut Vec<XMLNode>,
    deleted: &mut Vec<Element>,
    replacement: &str,
    insert_rpr: Option<&Element>,
    author: &str,
    date: Option<&str>,
    mint: &mut dyn FnMut() -> u32,
    tracked: bool,
) {
    if tracked {
        if !deleted.is_empty() {
            let mut del = w_el("del");
            stamp_revision(&mut del, author, date, mint());
            del.children = deleted.drain(..).map(XMLNode::Element).collect();
            out.push(XMLNode::Element(del));
        }
        if !replacement.is_empty() {
            let mut ins = w_el("ins");
            stamp_revision(&mut ins, author, date, mint());
            ins.children
                .push(XMLNode::Element(text_run(insert_rpr, replacement)));
            out.push(XMLNode::Element(ins));
        }
    } else {
        // Direct: the deleted runs simply vanish; a non-empty replacement becomes
        // one new run carrying the deleted span's formatting.
        deleted.clear();
        if !replacement.is_empty() {
            out.push(XMLNode::Element(text_run(insert_rpr, replacement)));
        }
    }
}

/// A `mint` closure that draws fresh ids from a whole-document counter.
fn counter_mint(rev_counter: &mut u32) -> impl FnMut() -> u32 + '_ {
    move || {
        let id = *rev_counter;
        *rev_counter += 1;
        id
    }
}

/// Stamp `w:id`/`w:author`/`w:date` onto a `w:ins`/`w:del`.
fn stamp_revision(el: &mut Element, author: &str, date: Option<&str>, id: u32) {
    crate::xml_attrs::attr_set(el, "w:id", id.to_string().as_str());
    crate::xml_attrs::attr_set(el, "w:author", author);
    if let Some(date) = date {
        crate::xml_attrs::attr_set(el, "w:date", date);
    }
}

/// `<w:r>[rPr]<w:t xml:space="preserve">text</w:t></w:r>`.
fn text_run(rpr: Option<&Element>, text: &str) -> Element {
    let mut r = w_el("r");
    if let Some(rpr) = rpr {
        r.children.push(XMLNode::Element(rpr.clone()));
    }
    let mut t = w_el("t");
    crate::xml_attrs::attr_set(&mut t, "xml:space", "preserve");
    t.children.push(XMLNode::Text(text.to_string()));
    r.children.push(XMLNode::Element(t));
    r
}

/// `<w:r>[rPr]<w:delText xml:space="preserve">text</w:delText></w:r>` — a deleted
/// run: the run wrapper stays (carrying its formatting), `w:t` becomes `w:delText`.
fn deleted_run(rpr: Option<&Element>, text: &str) -> Element {
    let mut r = w_el("r");
    if let Some(rpr) = rpr {
        r.children.push(XMLNode::Element(rpr.clone()));
    }
    let mut dt = w_el("delText");
    crate::xml_attrs::attr_set(&mut dt, "xml:space", "preserve");
    dt.children.push(XMLNode::Text(text.to_string()));
    r.children.push(XMLNode::Element(dt));
    r
}

fn clamp(x: usize, lo: usize, hi: usize) -> usize {
    x.max(lo).min(hi)
}

/// The first `w:p` in `root`'s subtree (self included) that carries any visible
/// `w:t` text, mutably — the text region a body-level content-control fill sets.
pub(crate) fn first_text_paragraph_mut(root: &mut Element) -> Option<&mut Element> {
    if crate::word_xml::is_w_tag(root, "p") {
        let mut text = String::new();
        crate::opaque_targets::collect_wt_text(root, &mut text);
        if !text.is_empty() {
            return Some(root);
        }
    }
    for child in &mut root.children {
        if let XMLNode::Element(c) = child
            && let Some(found) = first_text_paragraph_mut(c)
        {
            return Some(found);
        }
    }
    None
}

/// The first descendant (or self) with local name `local`, mutably.
pub(crate) fn first_descendant_mut<'a>(
    el: &'a mut Element,
    local: &str,
) -> Option<&'a mut Element> {
    if crate::word_xml::is_w_tag(el, local) {
        return Some(el);
    }
    for child in &mut el.children {
        if let XMLNode::Element(c) = child
            && let Some(found) = first_descendant_mut(c, local)
        {
            return Some(found);
        }
    }
    None
}

/// The region's visible text IFF it is cleanly whole-value FILLABLE — every bit
/// of its text lives in DIRECT simple runs, with no barrier child (hyperlink /
/// field / nested control) hiding text a fill could not delete in place. `None`
/// when a fill would relocate or duplicate barrier text. Discovery uses this so it
/// only advertises controls `sdt_text_fill` can set faithfully (matching the
/// `set_region_text` guard).
pub(crate) fn fillable_text(region: &Element) -> Option<String> {
    let mut text = String::new();
    for c in &region.children {
        match text_run_parts(c) {
            Some((_, t)) => text.push_str(&t),
            None => {
                if let XMLNode::Element(e) = c
                    && element_has_wt_text(e)
                {
                    return None; // barrier text — not fillable in place
                }
            }
        }
    }
    Some(text)
}

/// Whether `e` (or any descendant) carries visible `w:t` text — used to detect a
/// barrier child (hyperlink/field/nested control) that hides fillable text.
fn element_has_wt_text(e: &Element) -> bool {
    if crate::word_xml::is_w_tag(e, "t") {
        return e
            .children
            .iter()
            .any(|c| matches!(c, XMLNode::Text(t) | XMLNode::CData(t) if !t.is_empty()));
    }
    e.children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(ce) if element_has_wt_text(ce)))
}

/// A tracked-change container we must not splice across.
fn is_tracked_container(e: &Element) -> bool {
    crate::word_xml::is_w_tag(e, "ins")
        || crate::word_xml::is_w_tag(e, "del")
        || crate::word_xml::is_w_tag(e, "moveFrom")
        || crate::word_xml::is_w_tag(e, "moveTo")
}

/// The visible `w:t` text of a child IF it is a plain text run — a `w:r` whose
/// only content child is a single `w:t` (an optional `w:rPr` is allowed). Any
/// other run shape (tab, break, drawing, multiple/zero `w:t`) returns `None` and
/// is treated as a non-text barrier.
fn text_run_text(child: &XMLNode) -> Option<String> {
    text_run_parts(child).map(|(_, t)| t)
}

fn text_run_parts(child: &XMLNode) -> Option<(Option<&Element>, String)> {
    let XMLNode::Element(r) = child else {
        return None;
    };
    if !crate::word_xml::is_w_tag(r, "r") {
        return None;
    }
    let mut rpr: Option<&Element> = None;
    let mut t: Option<&Element> = None;
    for c in &r.children {
        let XMLNode::Element(e) = c else { continue };
        if crate::word_xml::is_w_tag(e, "rPr") {
            rpr = Some(e);
        } else if crate::word_xml::is_w_tag(e, "t") {
            if t.is_some() {
                return None; // more than one w:t → not a simple text run
            }
            t = Some(e);
        } else {
            return None; // any other content (tab/br/drawing/…) → barrier
        }
    }
    let t = t?;
    let mut text = String::new();
    for c in &t.children {
        if let XMLNode::Text(s) | XMLNode::CData(s) = c {
            text.push_str(s);
        }
    }
    Some((rpr, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment};

    fn para(inner: &str) -> Element {
        parse_raw_fragment(format!("<w:p>{inner}</w:p>").as_bytes()).unwrap()
    }

    fn rev() -> RevisionInfo {
        RevisionInfo {
            revision_id: 0,
            author: Some("Ada".to_string()),
            date: Some("2024-01-01T00:00:00Z".to_string()),
            apply_op_id: None,
        }
    }

    fn serialized(e: &Element) -> String {
        String::from_utf8(serialize_raw_fragment(e)).unwrap()
    }

    #[test]
    fn tracked_partial_replace_within_one_run() {
        let mut p = para(r#"<w:r><w:t>The quick brown fox</w:t></w:r>"#);
        let mut counter = 5;
        splice_region_text(&mut p, "quick", "slow", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        // old word deleted (delText), new word inserted (w:t inside w:ins).
        assert!(out.contains("<w:del"), "expected a w:del: {out}");
        assert!(out.contains("delText") && out.contains("quick"), "{out}");
        assert!(out.contains("<w:ins") && out.contains("slow"), "{out}");
        // Surrounding text preserved as its own runs.
        assert!(out.contains("The ") && out.contains(" brown fox"), "{out}");
        // Two fresh ids minted (del + ins).
        assert_eq!(counter, 7);
    }

    #[test]
    fn direct_replace_produces_no_tracked_markup() {
        let mut p = para(r#"<w:r><w:t>Hello world</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, "world", "there", &rev(), &mut counter, false).unwrap();
        let out = serialized(&p);
        assert!(!out.contains("<w:ins") && !out.contains("<w:del"), "{out}");
        assert!(out.contains("there") && !out.contains("world"), "{out}");
        assert_eq!(counter, 1, "direct mode mints no revision ids");
    }

    #[test]
    fn span_across_two_runs() {
        let mut p = para(r#"<w:r><w:t>foo </w:t></w:r><w:r><w:t>bar baz</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, "foo bar", "X", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        assert!(out.contains("delText"), "{out}");
        assert!(out.contains("<w:ins") && out.contains(">X<"), "{out}");
        assert!(out.contains(" baz"), "trailing text kept: {out}");
    }

    #[test]
    fn missing_text_refuses() {
        let mut p = para(r#"<w:r><w:t>abc</w:t></w:r>"#);
        let mut counter = 1;
        assert_eq!(
            splice_region_text(&mut p, "xyz", "q", &rev(), &mut counter, true),
            Err(SpliceError::TextNotFound)
        );
    }

    #[test]
    fn preexisting_tracked_change_refuses() {
        let mut p =
            para(r#"<w:r><w:t>a</w:t></w:r><w:ins w:id="1"><w:r><w:t>b</w:t></w:r></w:ins>"#);
        let mut counter = 1;
        assert_eq!(
            splice_region_text(&mut p, "a", "z", &rev(), &mut counter, true),
            Err(SpliceError::RegionHasTrackedChanges)
        );
    }

    #[test]
    fn edits_text_inside_a_hyperlink() {
        // "click here" is inside a hyperlink; editing "here"→"there" must splice
        // INSIDE the hyperlink (wrapper preserved) — Word edits links freely.
        let mut p = para(
            r#"<w:r><w:t>See </w:t></w:r><w:hyperlink r:id="rId1"><w:r><w:t>click here</w:t></w:r></w:hyperlink>"#,
        );
        let mut counter = 1;
        splice_region_text(&mut p, "here", "there", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        // The del/ins live INSIDE the still-present hyperlink.
        let hl = out.find("<w:hyperlink").expect("hyperlink preserved");
        let hl_end = out.find("</w:hyperlink>").unwrap();
        assert!(
            out[hl..hl_end].contains("<w:ins") && out[hl..hl_end].contains("there"),
            "{out}"
        );
        assert!(
            out[hl..hl_end].contains("delText") && out[hl..hl_end].contains("here"),
            "{out}"
        );
        assert!(out.contains("See "), "surrounding text kept: {out}");
    }

    #[test]
    fn edit_straddling_a_hyperlink_boundary_refuses() {
        // "See click" spans the plain run AND into the hyperlink — a straddle.
        let mut p = para(
            r#"<w:r><w:t>See </w:t></w:r><w:hyperlink r:id="rId1"><w:r><w:t>click here</w:t></w:r></w:hyperlink>"#,
        );
        let mut counter = 1;
        assert_eq!(
            splice_region_text(&mut p, "See click", "x", &rev(), &mut counter, true),
            Err(SpliceError::TextNotFound)
        );
    }

    #[test]
    fn span_crossing_barrier_refuses() {
        // "ab" then a break then "cd"; span "abcd" crosses the <w:br/>.
        let mut p = para(r#"<w:r><w:t>ab</w:t></w:r><w:r><w:br/></w:r><w:r><w:t>cd</w:t></w:r>"#);
        let mut counter = 1;
        assert_eq!(
            splice_region_text(&mut p, "abcd", "z", &rev(), &mut counter, true),
            Err(SpliceError::UnsupportedRegionShape)
        );
    }

    #[test]
    fn set_region_replaces_whole_value_tracked() {
        let mut p = para(r#"<w:r><w:t>Acme Corp</w:t></w:r>"#);
        let mut counter = 1;
        set_region_text(&mut p, "Globex Inc", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        assert!(
            out.contains("delText") && out.contains("Acme Corp"),
            "{out}"
        );
        assert!(
            out.contains("<w:ins") && out.contains("Globex Inc"),
            "{out}"
        );
    }

    #[test]
    fn set_region_direct_leaves_only_value() {
        let mut p = para(r#"<w:r><w:t>old</w:t></w:r>"#);
        let mut counter = 1;
        set_region_text(&mut p, "new", &rev(), &mut counter, false).unwrap();
        let out = serialized(&p);
        assert!(!out.contains("<w:ins") && !out.contains("<w:del"), "{out}");
        assert!(out.contains("new") && !out.contains("old"), "{out}");
    }

    #[test]
    fn set_region_into_empty_inserts_value() {
        let mut p = para("");
        let mut counter = 3;
        set_region_text(&mut p, "filled", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        assert!(out.contains("<w:ins") && out.contains("filled"), "{out}");
    }

    #[test]
    fn replacement_preserves_run_formatting() {
        let mut p = para(r#"<w:r><w:rPr><w:b/></w:rPr><w:t>bold word</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, "word", "text", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        // The inserted run carries the deleted run's rPr (bold).
        assert!(out.contains("<w:ins"), "{out}");
        let ins_start = out.find("<w:ins").unwrap();
        assert!(
            out[ins_start..].contains("<w:b"),
            "inserted run keeps bold: {out}"
        );
    }

    /// Entity escaping round-trips: text containing `&`, `<`, and quotes must
    /// match against the DECODED text and re-serialize re-escaped — never
    /// double-escaped, never raw.
    #[test]
    fn entity_escaping_round_trips_through_splice() {
        let mut p = para(r#"<w:r><w:t>Smith &amp; Sons &lt;LLC&gt;</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(
            &mut p,
            "Smith & Sons",
            "Smith & Daughters",
            &rev(),
            &mut counter,
            true,
        )
        .unwrap();
        let out = serialized(&p);
        assert!(
            out.contains("Smith &amp; Daughters"),
            "inserted text re-escaped exactly once: {out}"
        );
        assert!(
            out.contains("&lt;LLC&gt;"),
            "untouched tail keeps its escaping: {out}"
        );
        assert!(!out.contains("&amp;amp;"), "no double escaping: {out}");
        // Round-trip: the output parses back and the visible text is decoded.
        let reparsed = parse_raw_fragment(out.as_bytes()).expect("output must reparse");
        let mut text = String::new();
        crate::opaque_targets::collect_wt_text(&reparsed, &mut text);
        assert!(text.contains("Smith & Daughters"), "{text}");
    }

    /// Empty replacement = pure deletion: tracked mode emits a w:del and NO
    /// w:ins; direct mode simply removes the text.
    #[test]
    fn empty_replacement_is_pure_deletion() {
        let mut p = para(r#"<w:r><w:t>keep DROP keep</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, " DROP", "", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        assert!(out.contains("<w:del") && out.contains("DROP"), "{out}");
        assert!(
            !out.contains("<w:ins"),
            "no empty insertion authored: {out}"
        );

        let mut p = para(r#"<w:r><w:t>keep DROP keep</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, " DROP", "", &rev(), &mut counter, false).unwrap();
        let out = serialized(&p);
        assert!(!out.contains("DROP") && !out.contains("<w:del"), "{out}");
        let reparsed = parse_raw_fragment(out.as_bytes()).unwrap();
        let mut text = String::new();
        crate::opaque_targets::collect_wt_text(&reparsed, &mut text);
        assert_eq!(text, "keep keep", "visible text after direct deletion");
    }

    /// Multi-byte UTF-8 in the find/replace and surrounding text: the
    /// char/byte offset conversion must not split a code point.
    #[test]
    fn multibyte_utf8_find_and_replace() {
        let mut p = para(r#"<w:r><w:t>Über die Brücke — größte Härte</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, "größte", "kleinste", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        assert!(out.contains("größte") && out.contains("kleinste"), "{out}");
        assert!(out.contains("Über die Brücke"), "prefix intact: {out}");
        assert!(out.contains("Härte"), "suffix intact: {out}");
    }

    /// w:smartTag is a transparent wrapper like w:hyperlink: the splice
    /// descends into it and the tracked markup lands INSIDE the wrapper.
    #[test]
    fn smart_tag_interior_is_editable() {
        let mut p = para(
            r#"<w:r><w:t xml:space="preserve">Call </w:t></w:r><w:smartTag w:uri="urn:x" w:element="phone"><w:r><w:t>555-0100</w:t></w:r></w:smartTag>"#,
        );
        let mut counter = 1;
        splice_region_text(&mut p, "555-0100", "555-0199", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        let tag = out
            .split("<w:smartTag")
            .nth(1)
            .and_then(|s| s.split("</w:smartTag>").next())
            .expect("smartTag survives");
        assert!(
            tag.contains("<w:ins") && tag.contains("555-0199"),
            "tracked change lands inside the wrapper: {out}"
        );
    }

    /// Significant leading/trailing whitespace survives the splice: split-run
    /// halves that begin or end with a space still round-trip their text.
    #[test]
    fn significant_whitespace_survives_split_runs() {
        let mut p = para(r#"<w:r><w:t xml:space="preserve">  lead mid trail  </w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, "mid", "MID", &rev(), &mut counter, false).unwrap();
        let reparsed = parse_raw_fragment(serialized(&p).as_bytes()).unwrap();
        let mut text = String::new();
        crate::opaque_targets::collect_wt_text(&reparsed, &mut text);
        assert_eq!(text, "  lead MID trail  ", "whitespace preserved exactly");
    }

    /// A barrier ADJACENT to the span (immediately before or after, not
    /// crossed) must not block the splice and must survive in place.
    #[test]
    fn barrier_adjacent_to_span_is_preserved() {
        let mut p = para(r#"<w:r><w:t>ab</w:t></w:r><w:r><w:br/></w:r><w:r><w:t>cd</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, "cd", "CD", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        assert!(out.contains("<w:br"), "adjacent barrier preserved: {out}");
        assert!(out.contains("CD") && out.contains("ab"), "{out}");

        let mut p = para(r#"<w:r><w:t>ab</w:t></w:r><w:r><w:br/></w:r><w:r><w:t>cd</w:t></w:r>"#);
        let mut counter = 1;
        splice_region_text(&mut p, "ab", "AB", &rev(), &mut counter, true).unwrap();
        let out = serialized(&p);
        assert!(out.contains("<w:br"), "{out}");
        assert!(out.contains("AB") && out.contains("cd"), "{out}");
    }
}
