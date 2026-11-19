//! Bookmarks are annotations, as in Word's Compare.
//!
//! A bookmark (`w:bookmarkStart` / `w:bookmarkEnd`, ISO 29500-1 §17.13.6.2)
//! is identified by its name, and a name is unique in a document. When the
//! base's bookmarked text is deleted in one place and the target's text
//! carrying the same bookmark is inserted in another (a paragraph moved and
//! edited), the redline holds both copies of the name, and the serializer
//! keeps one: by default the base copy, which sits inside the deletion, so
//! Accept All would lose a bookmark the target has, and every field that
//! refers to it.
//!
//! Word's Compare places such a bookmark on the target's copy. So does this
//! comparison: when a bookmark name's only base copy lies wholly (both
//! halves) in deleted content and its target copy wholly in inserted
//! content, the base copy is left out of the redline. Accept All then holds every bookmark of the
//! target; Reject All keeps the base's text without that one bookmark name,
//! as Word's own result does (a named departure from the Reject All law,
//! limited to the bookmark).
//!
//! Moves (`w:moveFrom` / `w:moveTo`) are left alone: their bookmark halves
//! are paired by the move carrier.

use std::collections::HashSet;

use stemma::domain::{
    BlockNode, CanonDoc, DecorationNode, DecorationType, InlineNode, TrackedBlock, TrackingStatus,
};

/// How a bookmark marker sits in the redline: its effective tracking (from
/// its block, row, cell and segment) and the side it was written from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// Base content the redline deletes.
    DeletedBase,
    /// Target content the redline inserts.
    InsertedTarget,
    Other,
}

/// A bookmark marker: start or end, its `w:id`, and a start's `w:name`.
struct Marker {
    is_start: bool,
    id: String,
    name: Option<String>,
}

fn marker(decoration: &DecorationNode) -> Option<Marker> {
    if decoration.kind != DecorationType::Bookmark {
        return None;
    }
    let element = xmltree::Element::parse(decoration.raw_xml.as_deref()?).ok()?;
    let is_start = match element.name.as_str() {
        "bookmarkStart" => true,
        "bookmarkEnd" => false,
        _ => return None,
    };
    Some(Marker {
        is_start,
        id: element.get_attribute("id")?.clone(),
        name: element.get_attribute("name").cloned(),
    })
}

/// The side a decoration is written from, as the serializer decides it: its
/// own origin, else its block's (an inserted block is target content).
fn is_target(decoration: &DecorationNode, block_is_target: bool) -> bool {
    match decoration.origin.as_deref() {
        Some(origin) => origin == "target",
        None => block_is_target,
    }
}

fn placement(status: &TrackingStatus, target: bool) -> Placement {
    match (status, target) {
        (TrackingStatus::Deleted(_), false) => Placement::DeletedBase,
        (TrackingStatus::Inserted(_), true) => Placement::InsertedTarget,
        _ => Placement::Other,
    }
}

/// The status inside `outer`: an untracked container takes its part's own.
fn within<'a>(outer: &'a TrackingStatus, own: Option<&'a TrackingStatus>) -> &'a TrackingStatus {
    match (outer, own) {
        (TrackingStatus::Normal, Some(own)) => own,
        _ => outer,
    }
}

/// Visit every bookmark decoration of `block` with its placement; `keep`
/// decides whether it stays.
fn visit_block(
    block: &mut BlockNode,
    status: &TrackingStatus,
    block_is_target: bool,
    visit: &mut dyn FnMut(&Marker, bool, Placement) -> bool,
) {
    match block {
        BlockNode::Paragraph(paragraph) => {
            for segment in &mut paragraph.segments {
                let status = within(status, Some(&segment.status)).clone();
                segment.inlines.retain(|inline| {
                    let InlineNode::Decoration(decoration) = inline else {
                        return true;
                    };
                    let Some(marker) = marker(decoration) else {
                        return true;
                    };
                    let target = is_target(decoration, block_is_target);
                    visit(&marker, target, placement(&status, target))
                });
            }
        }
        BlockNode::Table(table) => {
            for row in &mut table.rows {
                let status = within(status, row.tracking_status.as_ref()).clone();
                for cell in &mut row.cells {
                    let status = within(&status, cell.tracking_status.as_ref()).clone();
                    for inner in &mut cell.blocks {
                        visit_block(inner, &status, block_is_target, visit);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

fn visit_story(
    blocks: &mut [TrackedBlock],
    visit: &mut dyn FnMut(&Marker, bool, Placement) -> bool,
) {
    for tracked in blocks {
        if tracked.move_id.is_some() {
            continue;
        }
        let block_is_target = matches!(tracked.status, TrackingStatus::Inserted(_));
        let status = tracked.status.clone();
        visit_block(&mut tracked.block, &status, block_is_target, visit);
    }
}

/// Leave out the base copy of each bookmark whose base copy lies wholly in
/// deleted content and whose target copy lies wholly in inserted content, in
/// one story (see the module documentation). A bookmark whose either copy is
/// split across kept and changed content, or whose name has another base
/// copy, is left as it is.
fn place_story_bookmarks_on_target(blocks: &mut [TrackedBlock]) {
    // Pass 1: every bookmark half, with its side and placement.
    struct Half {
        is_start: bool,
        id: String,
        name: Option<String>,
        target: bool,
        placement: Placement,
    }
    let mut halves = Vec::new();
    visit_story(blocks, &mut |marker, target, placement| {
        halves.push(Half {
            is_start: marker.is_start,
            id: marker.id.clone(),
            name: marker.name.clone(),
            target,
            placement,
        });
        true
    });
    // A pair (one side's halves of one id) lies wholly in `placement`.
    let wholly = |target: bool, id: &str, placement: Placement| {
        let pair: Vec<&Half> = halves
            .iter()
            .filter(|half| half.target == target && half.id == id)
            .collect();
        pair.iter().filter(|half| half.is_start).count() == 1
            && pair.iter().filter(|half| !half.is_start).count() == 1
            && pair.iter().all(|half| half.placement == placement)
    };
    let base_starts = |name: &str| {
        halves
            .iter()
            .filter(|half| !half.target && half.is_start && half.name.as_deref() == Some(name))
            .count()
    };
    let target_names: HashSet<&str> = halves
        .iter()
        .filter(|half| half.target && half.is_start)
        .filter(|half| wholly(true, &half.id, Placement::InsertedTarget))
        .filter_map(|half| half.name.as_deref())
        .collect();
    let dropped_ids: HashSet<String> = halves
        .iter()
        .filter(|half| !half.target && half.is_start)
        .filter(|half| {
            half.name
                .as_deref()
                .is_some_and(|name| target_names.contains(name) && base_starts(name) == 1)
        })
        .filter(|half| wholly(false, &half.id, Placement::DeletedBase))
        .map(|half| half.id.clone())
        .collect();
    if dropped_ids.is_empty() {
        return;
    }
    // Pass 2: drop both halves of those base bookmarks.
    visit_story(blocks, &mut |marker, target, placement| {
        target || placement != Placement::DeletedBase || !dropped_ids.contains(&marker.id)
    });
}

/// Apply [`place_story_bookmarks_on_target`] to every story of the redline.
pub(crate) fn place_moved_bookmarks_on_target(merged: &mut CanonDoc) {
    place_story_bookmarks_on_target(&mut merged.blocks);
    for story in &mut merged.headers {
        place_story_bookmarks_on_target(&mut story.blocks);
    }
    for story in &mut merged.footers {
        place_story_bookmarks_on_target(&mut story.blocks);
    }
    for story in &mut merged.footnotes {
        place_story_bookmarks_on_target(&mut story.blocks);
    }
    for story in &mut merged.endnotes {
        place_story_bookmarks_on_target(&mut story.blocks);
    }
}
