//! Comment authoring verbs (§17.13.4 comments; MS-DOCX §2.5.1 commentsExtended).
//! "Comment on this clause; reply to that comment; mark it resolved; delete it."
//!
//! Comments are **annotations, not tracked changes**. The three anchor markers
//! (`commentRangeStart` / `commentRangeEnd` / `commentReference`, §17.13.4.4 /
//! .5 / .6) are spliced into the body as **zero-width `Normal` decorations**
//! even when the transaction is in `TrackedChange` mode — they are NEVER
//! wrapped in `w:ins` / `w:del`. The comment story itself lives in
//! `doc.comments` (word/comments.xml) and is likewise not a redline. Only a
//! co-located *text* edit would be a tracked change, and that is a separate
//! verb. Consequently accept-all and reject-all both leave the comment story
//! and its markers intact.
//!
//! This verb does **not** touch the materializer (Invariant M): like
//! `fields_crossrefs`, it splices zero-width markers directly into the target
//! paragraph's segments and pushes a story. `CommentReply` likewise splices
//! zero-width markers — a reply carries its OWN anchor range at the parent's
//! span (see the reply-threading note below) — and pushes a story plus a
//! `comments_extended` record. `CommentResolve` touches only the
//! `comments_extended` sidecar (a flat typed list).
//!
//! ## Reply threading (MS-DOCX §2.5.1 commentsExtended + own anchor markers)
//!
//! A reply is NOT anchor-free. Real Word gives every reply its own
//! `commentRangeStart` / `commentRangeEnd` around the parent's anchored span
//! plus its own `commentReference` run — without them Word's Comments
//! collection never surfaces the reply (invisible in the UI and to COM
//! automation: silent data loss). `CommentReply` therefore locates the
//! parent's markers and splices the reply's beside them in Word's shape: the
//! reply's `commentRangeStart` just after the parent's start; the reply's
//! `commentRangeEnd` and `commentReference` just after the parent's reference
//! run. If the parent carries no anchor markers, the reply is refused
//! (`CommentParentUnanchored`)
//! rather than authored unreachable — the same no-half-* discipline as delete.
//! Threading in `commentsExtended` links the reply's `w14:paraId` to the
//! parent's via `w15:paraIdParent`; a reply-of-reply nests against its
//! immediate parent's markers and threads under it.
//!
//! ## commentsExtended keying + thread-level resolve (MS-DOCX §2.5.1)
//!
//! A `commentEx` record keys on a comment's **LAST body-paragraph** `w14:paraId`
//! (not the first — they coincide only for single-paragraph comments). Every
//! join here — resolve lookup, reply `paraIdParent`, delete subtree — therefore
//! uses [`CommentStory::last_para_id`](crate::domain::CommentStory::last_para_id).
//! Keying on the first paragraph would miss a multi-paragraph comment's real
//! record.
//!
//! `w15:done` is a **thread** property: Word derives a comment's resolved state
//! from the thread ROOT's record (the record with no `paraIdParent`).
//! `CommentResolve` on any comment in a thread walks up to the root and sets
//! `done` across the whole thread (root + all replies), matching Word — it never
//! synthesizes a duplicate orphan record keyed on the wrong paraId. If the
//! comment has no record at all, one is created keyed correctly on its last
//! paragraph (the documented create-if-none behavior).
//!
//! ## Commenting a redlined paragraph (tracked-anchor granularity)
//!
//! A comment may anchor on a paragraph that carries tracked *segments* — the
//! natural negotiation order is "make a tracked counter-edit on a clause, THEN
//! comment on that clause", and real Word annotates redlined clauses constantly
//! (§17.13.4). The block-level guard is therefore narrowed for `CommentCreate`:
//! only a whole INSERTED / DELETED / moved *block* is refused (its very
//! existence is contested); tracked segments are fine.
//!
//! The `expect` span resolves against the paragraph's VISIBLE text — the
//! concatenation of `Normal` and `Inserted` segment text (a `w:del` /
//! inserted-then-deleted segment is struck, so it is NOT part of the anchor
//! space). Two refusals remain, both actionable:
//! - an anchor that cannot be located → `CommentAnchorNotFound`;
//! - an anchor whose text falls on DELETED content → `CommentAnchorOverlapsDeleted`.
//!
//! Range-marker placement lands on segment boundaries. A comment range may
//! legally ENCLOSE whole `w:ins` / `w:del` containers that sit between its
//! endpoints, but a marker must never split a tracked container mid-run: when a
//! resolved endpoint falls inside an `Inserted` segment we WIDEN the range
//! outward to that segment's boundary (enclosing the whole `w:ins`) and drop the
//! marker in its own zero-width `Normal` decoration segment beside it. Endpoints
//! inside a `Normal` segment split its text as before. This keeps the serialized
//! output clean under the annotation-ordering validator (I-ANN-005 / I-TC-001)
//! and keeps the markers paired through accept-all and reject-all: on reject-all
//! of an enclosed insertion the anchor text shrinks but both markers survive
//! (they are Normal), so the range simply collapses to the remaining anchor —
//! the comment is retained, never orphaned.
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - `CommentCreate` targets top-level body paragraphs only;
//! - the body must be non-empty (an empty comment is refused, not defaulted);
//! - `CommentDelete` deletes the whole discussion rooted at the target: the
//!   comment PLUS every reply that threads under it, transitively. For each
//!   deleted comment it removes the story + all three markers from EVERY
//!   story's blocks; if any marker is missing it fails `CommentRangeOrphaned`
//!   (no half-delete). This matches Word (deleting a thread's root deletes the
//!   thread) and keeps `commentsExtended` coherent — deleting a leaf reply
//!   removes only that reply.

use super::super::{EditError, find_block_index};
use crate::domain::{
    BlockNode, CanonDoc, CommentExtended, CommentStory, InlineNode, NodeId, OpaqueKind,
    ParagraphNode, RevisionInfo, TrackedBlock, TrackedSegment, TrackingStatus,
};
use crate::semantic_hash::check_block_guard;

/// A located comment span: TextNode run `[first..=last]` (contiguous in the
/// segment) whose concatenated text contains the match at char range
/// `[start, end)`. The range markers are spliced at those char offsets.
struct SpanPlan {
    seg_idx: usize,
    first: usize,
    last: usize,
    start: usize,
    end: usize,
}

/// Allocate a fresh comment `w:id` not colliding with any existing comment.
/// DOCX comment ids are integers; we pick `max + 1` (or 0 for the first).
fn fresh_comment_id(doc: &CanonDoc) -> String {
    let max = doc
        .comments
        .iter()
        .filter_map(|c| c.id.parse::<u64>().ok())
        .max();
    match max {
        Some(n) => (n + 1).to_string(),
        None => "0".to_string(),
    }
}

/// Allocate a fresh `w14:paraId` (8 hex digits, MS-DOCX §2.2.4) not colliding
/// with ANY existing comment paragraph's paraId (every paragraph, not just the
/// keying one, so a multi-paragraph comment can't collide) or any
/// commentsExtended record. Derives a deterministic value from the comment id
/// so a single apply is reproducible.
fn fresh_para_id(doc: &CanonDoc, comment_id: &str) -> String {
    let mut existing: std::collections::HashSet<String> = doc
        .comments
        .iter()
        .flat_map(|c| c.blocks.iter())
        .filter_map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.para_id.clone(),
            _ => None,
        })
        .collect();
    for r in &doc.comments_extended {
        existing.insert(r.para_id.clone());
        if let Some(p) = &r.para_id_parent {
            existing.insert(p.clone());
        }
    }
    // Seed from the comment id; bump on the (vanishingly unlikely) collision.
    let mut seed: u32 = 0x0C00_0000;
    for b in comment_id.bytes() {
        seed = seed.wrapping_mul(31).wrapping_add(b as u32);
    }
    loop {
        let candidate = format!("{seed:08X}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        seed = seed.wrapping_add(1);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_create(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    body: &str,
    author: Option<String>,
    revision: &RevisionInfo,
    step_index: usize,
) -> Result<(), EditError> {
    // No silent fallback: an empty comment body is meaningless, refuse it.
    if body.trim().is_empty() {
        return Err(EditError::CommentEmptyBody { step_index });
    }

    // v1: top-level body paragraphs only. A nested paragraph surfaces as
    // BlockNotFound here, matching the other anchor-splice verbs.
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    // Block-level tracked status still refuses (a whole inserted/deleted block's
    // existence is contested), but tracked SEGMENTS do NOT — a comment legally
    // anchors on a redlined clause. The refusal names the escape hatch.
    let block_status = match &doc.blocks[idx].status {
        TrackingStatus::Normal => None,
        TrackingStatus::Inserted(_) => Some("inserted"),
        TrackingStatus::Deleted(_) => Some("deleted"),
        TrackingStatus::InsertedThenDeleted(_) => Some("inserted_then_deleted"),
    };
    if let Some(status) = block_status {
        return Err(EditError::CommentOnTrackedBlock {
            block_id: block_id.clone(),
            status,
            step_index,
        });
    }

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

    // Allocate identity before borrowing the paragraph mutably.
    let comment_id = fresh_comment_id(doc);
    let para_id = fresh_para_id(doc, &comment_id);

    let BlockNode::Paragraph(para) = &mut doc.blocks[idx].block else {
        unreachable!("checked paragraph above");
    };

    // Fast path: the anchor lies within a single contiguous run of Text nodes in
    // one Normal segment. This is the common case and keeps the markers inside
    // that segment (no new segments), exactly as before tracked-anchor support.
    match find_span(&para.segments, expect) {
        Some(plan) => splice_markers(&mut para.segments, plan, &comment_id),
        None => {
            // The anchor crosses a segment boundary and/or touches an Inserted
            // segment. Resolve it against the paragraph's VISIBLE text and place
            // the range markers on segment boundaries (widening over whole
            // tracked containers).
            let cross = resolve_cross_segment_anchor(&para.segments, expect, block_id, step_index)?;
            apply_cross_segment_markers(&mut para.segments, cross, &comment_id);
        }
    }
    para.block_text_hash = None;
    para.rendered_text = None;

    // Build the comment story body. Its first paragraph carries the w14:paraId
    // that commentsExtended references (and resolve/reply key on).
    let body_para =
        ParagraphNode::new_story_body(&format!("cm_{comment_id}_p1"), body, Some(para_id.clone()));
    let blocks = vec![TrackedBlock {
        status: TrackingStatus::Normal,
        block: BlockNode::from(body_para),
        move_id: None,
        block_sdt_wrap: None,
    }];
    let content_hash = crate::import::compute_story_content_hash(&[blocks[0].block.clone()]);

    doc.comments.push(CommentStory {
        id: comment_id,
        author,
        date: revision.date.clone(),
        blocks,
        content_hash,
        tracking_status: None,
    });
    Ok(())
}

pub(crate) fn apply_reply(
    doc: &mut CanonDoc,
    parent_comment_id: &str,
    body: &str,
    author: Option<String>,
    revision: &RevisionInfo,
    step_index: usize,
) -> Result<(), EditError> {
    if body.trim().is_empty() {
        return Err(EditError::CommentEmptyBody { step_index });
    }

    // The parent must exist, and must have a first-body-paragraph paraId to
    // thread under (commentsExtended links by paraId, not comment id). If the
    // parent has no paraId we synthesize one on it so the thread is well-formed
    // — that is an explicit, in-scope repair, not a silent fallback.
    let parent_pos = doc
        .comments
        .iter()
        .position(|c| c.id == parent_comment_id)
        .ok_or_else(|| EditError::CommentTargetNotFound {
            comment_id: parent_comment_id.to_string(),
            step_index,
        })?;

    // A reply must carry its OWN anchor markers at the parent's span, or Word's
    // Comments collection never surfaces it (§17.13.4 / MS-DOCX §2.5.1): the
    // reply is invisible in the UI and to COM automation — silent data loss.
    // We place those markers beside the parent's, so the parent must be
    // anchored. Scan for the parent's three markers BEFORE mutating anything;
    // if any is missing, refuse loud (mirroring CommentDelete's no-half-delete
    // discipline) rather than author an unreachable reply.
    let mut p_start = false;
    let mut p_end = false;
    let mut p_ref = false;
    for blocks in all_story_blocks(doc) {
        for tb in blocks {
            scan_block_for_markers(
                &tb.block,
                parent_comment_id,
                &mut p_start,
                &mut p_end,
                &mut p_ref,
            );
        }
    }
    let mut missing: Vec<&'static str> = Vec::new();
    if !p_start {
        missing.push("commentRangeStart");
    }
    if !p_end {
        missing.push("commentRangeEnd");
    }
    if !p_ref {
        missing.push("commentReference");
    }
    if !missing.is_empty() {
        return Err(EditError::CommentParentUnanchored {
            parent_comment_id: parent_comment_id.to_string(),
            missing_markers: missing,
            step_index,
        });
    }

    // Thread by the parent's LAST-paragraph paraId — the key MS-DOCX §2.5.1
    // uses. For a single-paragraph parent this is its only paraId; for a
    // multi-paragraph parent, keying on the first paragraph would point
    // `paraIdParent` at a paraId no `commentEx` record uses, breaking the
    // thread. Synthesize the key on the last paragraph if the parent has none.
    let parent_para_id = match doc.comments[parent_pos].last_para_id() {
        Some(p) => p.to_string(),
        None => {
            let pid = fresh_para_id(doc, parent_comment_id);
            set_last_para_id(&mut doc.comments[parent_pos], &pid);
            pid
        }
    };
    // Ensure the parent is itself represented in commentsExtended (MS-DOCX
    // §2.5.1 lists a record per comment). Word threads by the parent's paraId,
    // and our thread-scoped delete keys on these records — a thread whose root
    // has no record would be half-represented. Its own `paraIdParent` stays
    // None (it is a thread root); a pre-existing record's parent link and done
    // flag are left as they are.
    if !doc
        .comments_extended
        .iter()
        .any(|r| r.para_id == parent_para_id)
    {
        doc.comments_extended.push(CommentExtended {
            para_id: parent_para_id.clone(),
            para_id_parent: None,
            done: false,
        });
    }

    let reply_id = fresh_comment_id(doc);
    let reply_para_id = fresh_para_id(doc, &reply_id);

    // Author the reply's own anchor markers at the parent's span, in Word's
    // threaded-reply shape: the reply's commentRangeStart immediately after the
    // parent's commentRangeStart, and its commentRangeEnd + commentReference
    // immediately after the parent's commentReference run. We verified the
    // parent's markers are present above, so both halves always land.
    let mut placed_start = false;
    let mut placed_ref = false;
    for blocks in all_story_blocks_mut(doc) {
        for tb in blocks.iter_mut() {
            splice_reply_markers_in_block(
                &mut tb.block,
                parent_comment_id,
                &reply_id,
                &mut placed_start,
                &mut placed_ref,
            );
        }
    }
    debug_assert!(
        placed_start && placed_ref,
        "parent markers were scanned present but the reply splice missed them"
    );

    let body_para = ParagraphNode::new_story_body(
        &format!("cm_{reply_id}_p1"),
        body,
        Some(reply_para_id.clone()),
    );
    let blocks = vec![TrackedBlock {
        status: TrackingStatus::Normal,
        block: BlockNode::from(body_para),
        move_id: None,
        block_sdt_wrap: None,
    }];
    let content_hash = crate::import::compute_story_content_hash(&[blocks[0].block.clone()]);

    doc.comments.push(CommentStory {
        id: reply_id,
        author,
        date: revision.date.clone(),
        blocks,
        content_hash,
        tracking_status: None,
    });
    doc.comments_extended.push(CommentExtended {
        para_id: reply_para_id,
        para_id_parent: Some(parent_para_id),
        done: false,
    });
    Ok(())
}

pub(crate) fn apply_resolve(
    doc: &mut CanonDoc,
    comment_id: &str,
    done: bool,
    step_index: usize,
) -> Result<(), EditError> {
    let pos = doc
        .comments
        .iter()
        .position(|c| c.id == comment_id)
        .ok_or_else(|| EditError::CommentTargetNotFound {
            comment_id: comment_id.to_string(),
            step_index,
        })?;

    // commentsExtended keys on the comment's LAST-body-paragraph paraId
    // (MS-DOCX §2.5.1) — NOT the first. Keying on the first paragraph misses
    // the real record of a multi-paragraph comment and pushes a duplicate
    // orphan keyed on the wrong paraId, leaving Word reading Done=false.
    // Ensure the comment has a key (synthesize on its last paragraph if absent
    // — an in-scope repair, not a silent fallback).
    let para_id = match doc.comments[pos].last_para_id() {
        Some(p) => p.to_string(),
        None => {
            let pid = fresh_para_id(doc, comment_id);
            set_last_para_id(&mut doc.comments[pos], &pid);
            pid
        }
    };

    // `done` is a THREAD property: Word derives a comment's resolved state from
    // the thread ROOT's record. Resolving any comment in a thread therefore
    // acts on the whole thread — walk up to the root, then set `done` on every
    // record in the thread (root + all replies), matching Word's own resolve.
    let root_key = crate::domain::thread_root_key(&doc.comments_extended, &para_id);

    // Collect the thread's record keys: the root plus every record reachable
    // downward through `para_id_parent` (a fixpoint, mirroring delete's
    // subtree walk). If the target has no record yet, the thread is just the
    // target itself.
    let mut thread_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    if doc.comments_extended.iter().any(|r| r.para_id == root_key) {
        thread_keys.insert(root_key.clone());
        loop {
            let mut added = false;
            for r in &doc.comments_extended {
                if let Some(parent) = &r.para_id_parent
                    && thread_keys.contains(parent)
                    && !thread_keys.contains(&r.para_id)
                {
                    thread_keys.insert(r.para_id.clone());
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
    }

    if thread_keys.is_empty() {
        // No record for this comment anywhere — create one keyed correctly on
        // its last paragraph (the documented create-if-none behavior). A lone
        // comment is its own thread root.
        doc.comments_extended.push(CommentExtended {
            para_id,
            para_id_parent: None,
            done,
        });
    } else {
        for rec in doc.comments_extended.iter_mut() {
            if thread_keys.contains(&rec.para_id) {
                rec.done = done;
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_delete(
    doc: &mut CanonDoc,
    comment_id: &str,
    step_index: usize,
) -> Result<(), EditError> {
    // The comment story must exist.
    let pos = doc
        .comments
        .iter()
        .position(|c| c.id == comment_id)
        .ok_or_else(|| EditError::CommentTargetNotFound {
            comment_id: comment_id.to_string(),
            step_index,
        })?;

    // Contract (documented, deliberate, test-covered): deleting a comment
    // deletes the whole discussion it roots — the comment itself PLUS every
    // reply that threads under it, transitively (MS-DOCX §2.5.1 threads via
    // commentsExtended `paraIdParent`). This matches Word, which deletes an
    // entire thread when its root is deleted, and it keeps the sidecar
    // coherent. The previous behavior nulled surviving replies' `paraIdParent`,
    // silently promoting them to phantom top-level comments the author never
    // wrote — an invalid state, not a deliberate default. Deleting a leaf reply
    // deletes just that reply (its subtree is itself); its markers go without
    // touching the parent's, because we key removal on each id independently.
    //
    // Step 1: collect the paraIds in the subtree rooted at the target via a
    // fixpoint over the parent links. Keys are LAST-paragraph paraIds (MS-DOCX
    // §2.5.1), the same key `commentsExtended` records and reply threading use.
    // A target with no paraId can have no replies (a reply keys its parent by
    // that paraId), so its subtree is just itself and this set stays empty.
    let root_para_id = doc.comments[pos].last_para_id().map(|s| s.to_string());
    let mut subtree_para_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(root) = &root_para_id {
        subtree_para_ids.insert(root.clone());
        loop {
            let mut added = false;
            for r in &doc.comments_extended {
                if let Some(parent) = &r.para_id_parent
                    && subtree_para_ids.contains(parent)
                    && !subtree_para_ids.contains(&r.para_id)
                {
                    subtree_para_ids.insert(r.para_id.clone());
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
    }

    // Step 2: the comment ids to delete — the target plus every comment whose
    // last-body-paragraph paraId (the record key) is in the subtree set.
    let mut delete_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    delete_ids.insert(comment_id.to_string());
    for c in &doc.comments {
        if let Some(pid) = c.last_para_id()
            && subtree_para_ids.contains(pid)
        {
            delete_ids.insert(c.id.clone());
        }
    }

    // Step 3: no-half-delete guard for EVERY comment being deleted. All three
    // anchor markers must be present; a half-present range is a corrupt state
    // we refuse rather than proceed through (CLAUDE.md: no silent fallback).
    for del_id in &delete_ids {
        let mut found_start = false;
        let mut found_end = false;
        let mut found_ref = false;
        for blocks in all_story_blocks(doc) {
            for tb in blocks {
                scan_block_for_markers(
                    &tb.block,
                    del_id,
                    &mut found_start,
                    &mut found_end,
                    &mut found_ref,
                );
            }
        }
        let mut missing: Vec<&'static str> = Vec::new();
        if !found_start {
            missing.push("commentRangeStart");
        }
        if !found_end {
            missing.push("commentRangeEnd");
        }
        if !found_ref {
            missing.push("commentReference");
        }
        if !missing.is_empty() {
            return Err(EditError::CommentRangeOrphaned {
                comment_id: del_id.clone(),
                missing_markers: missing,
                step_index,
            });
        }
    }

    // Step 4: all present — remove the markers for every deleted id everywhere.
    for blocks in all_story_blocks_mut(doc) {
        for tb in blocks.iter_mut() {
            for del_id in &delete_ids {
                remove_markers_in_block(&mut tb.block, del_id);
            }
        }
    }

    // Step 5: drop the stories and their commentsExtended records. Every
    // deleted comment's paraId is in `subtree_para_ids` (the target's root plus
    // the replies), so retaining by that set clears the whole thread's sidecar.
    doc.comments.retain(|c| !delete_ids.contains(&c.id));
    doc.comments_extended
        .retain(|r| !subtree_para_ids.contains(&r.para_id));
    Ok(())
}

// ─── marker splicing / scanning helpers ──────────────────────────────────────

/// Set (or replace) the `w14:paraId` on a comment story's LAST paragraph — the
/// paragraph MS-DOCX §2.5.1 keys the `commentEx` record on. Used to repair a
/// comment that carries no paraId before threading/resolving it.
fn set_last_para_id(comment: &mut CommentStory, para_id: &str) {
    for tb in comment.blocks.iter_mut().rev() {
        if let BlockNode::Paragraph(p) = &mut tb.block {
            p.para_id = Some(para_id.to_string());
            return;
        }
    }
}

/// Borrow each story's block vector immutably (body + footnotes + endnotes +
/// comments + headers + footers). Comment anchors can in principle live in any
/// story, so delete scans them all.
fn all_story_blocks(doc: &CanonDoc) -> Vec<&[TrackedBlock]> {
    let mut out: Vec<&[TrackedBlock]> = vec![doc.blocks.as_slice()];
    out.extend(doc.headers.iter().map(|s| s.blocks.as_slice()));
    out.extend(doc.footers.iter().map(|s| s.blocks.as_slice()));
    out.extend(doc.footnotes.iter().map(|s| s.blocks.as_slice()));
    out.extend(doc.endnotes.iter().map(|s| s.blocks.as_slice()));
    out.extend(doc.comments.iter().map(|s| s.blocks.as_slice()));
    out
}

fn all_story_blocks_mut(doc: &mut CanonDoc) -> Vec<&mut Vec<TrackedBlock>> {
    let mut out: Vec<&mut Vec<TrackedBlock>> = vec![&mut doc.blocks];
    out.extend(doc.headers.iter_mut().map(|s| &mut s.blocks));
    out.extend(doc.footers.iter_mut().map(|s| &mut s.blocks));
    out.extend(doc.footnotes.iter_mut().map(|s| &mut s.blocks));
    out.extend(doc.endnotes.iter_mut().map(|s| &mut s.blocks));
    out.extend(doc.comments.iter_mut().map(|s| &mut s.blocks));
    out
}

fn scan_block_for_markers(
    block: &BlockNode,
    comment_id: &str,
    start: &mut bool,
    end: &mut bool,
    reference: &mut bool,
) {
    match block {
        BlockNode::Paragraph(p) => {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    match inline {
                        InlineNode::CommentRangeStart { id } if id == comment_id => *start = true,
                        InlineNode::CommentRangeEnd { id } if id == comment_id => *end = true,
                        // The reference is either a freshly-authored zero-width
                        // marker or (after import) an opaque run-level element;
                        // both carry the comment id.
                        InlineNode::CommentReference { id } if id == comment_id => {
                            *reference = true
                        }
                        InlineNode::OpaqueInline(o) => {
                            if let OpaqueKind::CommentReference(rd) = &o.kind
                                && rd.reference_id == comment_id
                            {
                                *reference = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        BlockNode::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    for b in &cell.blocks {
                        scan_block_for_markers(b, comment_id, start, end, reference);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// True when `inline` is any of the three anchor markers for `comment_id`
/// (range start/end, or the reference in either its zero-width or opaque form).
fn is_marker_for(inline: &InlineNode, comment_id: &str) -> bool {
    match inline {
        InlineNode::CommentRangeStart { id }
        | InlineNode::CommentRangeEnd { id }
        | InlineNode::CommentReference { id } => id == comment_id,
        InlineNode::OpaqueInline(o) => {
            matches!(&o.kind, OpaqueKind::CommentReference(rd) if rd.reference_id == comment_id)
        }
        _ => false,
    }
}

fn remove_markers_in_block(block: &mut BlockNode, comment_id: &str) {
    match block {
        BlockNode::Paragraph(p) => {
            let mut changed = false;
            for seg in &mut p.segments {
                let before = seg.inlines.len();
                seg.inlines
                    .retain(|inline| !is_marker_for(inline, comment_id));
                if seg.inlines.len() != before {
                    changed = true;
                }
            }
            // Drop now-empty segments left behind by removed markers.
            p.segments.retain(|s| !s.inlines.is_empty());
            if changed {
                p.block_text_hash = None;
                p.rendered_text = None;
            }
        }
        BlockNode::Table(t) => {
            for row in &mut t.rows {
                for cell in &mut row.cells {
                    for b in &mut cell.blocks {
                        remove_markers_in_block(b, comment_id);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// True when `inline` is the `commentReference` for `id` — either the
/// freshly-authored zero-width form or the opaque run-level element after an
/// import round-trip (both carry the comment id).
fn is_reference_for(inline: &InlineNode, id: &str) -> bool {
    match inline {
        InlineNode::CommentReference { id: i } => i == id,
        InlineNode::OpaqueInline(o) => {
            matches!(&o.kind, OpaqueKind::CommentReference(rd) if rd.reference_id == id)
        }
        _ => false,
    }
}

/// Splice a reply's own anchor markers beside the parent's, following Word's
/// threaded-reply shape (MS-DOCX §2.5.1): the reply's `commentRangeStart`
/// immediately after the parent's `commentRangeStart`, and its
/// `commentRangeEnd` + `commentReference` immediately after the parent's
/// `commentReference` run. The reply thus wraps the same anchored span as the
/// parent and carries a reference run of its own, which is what makes it
/// visible in Word. Sets `placed_start` / `placed_ref` as each marker is
/// emitted. The reply markers are zero-width `Normal` decorations, never
/// wrapped in `w:ins` / `w:del` — comments are annotations, not redlines.
fn splice_reply_markers_in_block(
    block: &mut BlockNode,
    parent_id: &str,
    reply_id: &str,
    placed_start: &mut bool,
    placed_ref: &mut bool,
) {
    match block {
        BlockNode::Paragraph(p) => {
            let mut changed = false;
            for seg in &mut p.segments {
                let old = std::mem::take(&mut seg.inlines);
                let mut out = Vec::with_capacity(old.len() + 2);
                for inline in old {
                    let after_start =
                        matches!(&inline, InlineNode::CommentRangeStart { id } if id == parent_id);
                    let after_ref = is_reference_for(&inline, parent_id);
                    out.push(inline);
                    if after_start {
                        out.push(InlineNode::CommentRangeStart {
                            id: reply_id.to_string(),
                        });
                        *placed_start = true;
                        changed = true;
                    }
                    if after_ref {
                        out.push(InlineNode::CommentRangeEnd {
                            id: reply_id.to_string(),
                        });
                        out.push(InlineNode::CommentReference {
                            id: reply_id.to_string(),
                        });
                        *placed_ref = true;
                        changed = true;
                    }
                }
                seg.inlines = out;
            }
            if changed {
                p.block_text_hash = None;
                p.rendered_text = None;
            }
        }
        BlockNode::Table(t) => {
            for row in &mut t.rows {
                for cell in &mut row.cells {
                    for b in &mut cell.blocks {
                        splice_reply_markers_in_block(
                            b,
                            parent_id,
                            reply_id,
                            placed_start,
                            placed_ref,
                        );
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// Locate `expect` within a single contiguous run of `Text` nodes in one Normal
/// segment. Mirrors `run_formatting::find_span` but carries the segment index.
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
            if let Some((start, end)) = char_find(&concat, expect) {
                return Some(SpanPlan {
                    seg_idx,
                    first: i,
                    last: j - 1,
                    start,
                    end,
                });
            }
            i = j.max(i + 1);
        }
    }
    None
}

/// Byte `find` mapped to a `[start, end)` char-offset range.
fn char_find(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let byte = haystack.find(needle)?;
    let start = haystack[..byte].chars().count();
    let end = start + needle.chars().count();
    Some((start, end))
}

/// Splice `commentRangeStart` before the matched span and
/// `commentRangeEnd` + `commentReference` after it, splitting the boundary text
/// nodes so the markers land exactly at the span edges. The markers are
/// inserted into the host (Normal) segment in place — they are zero-width
/// annotations, never their own Inserted/Deleted segment.
fn splice_markers(segments: &mut [TrackedSegment], plan: SpanPlan, comment_id: &str) {
    let seg = &mut segments[plan.seg_idx];
    let inlines = &mut seg.inlines;

    // Rebuild the run [first..=last] with the span split out, inserting the
    // markers at the span boundaries.
    let mut rebuilt: Vec<InlineNode> = Vec::new();
    let mut offset = 0usize;
    let drained: Vec<InlineNode> = inlines.drain(plan.first..=plan.last).collect();
    for inline in drained {
        let InlineNode::Text(node) = inline else {
            unreachable!("run is all TextNodes");
        };
        let node_len = node.text.chars().count();
        let node_start = offset;
        let node_end = offset + node_len;
        offset = node_end;

        let chars: Vec<char> = node.text.chars().collect();
        // Char offsets within this node where the span boundaries fall.
        let local_start = plan.start.saturating_sub(node_start);
        let local_end = plan.end.saturating_sub(node_start);

        // Emit the node, inserting markers at the boundary offsets that fall
        // inside (or exactly at the edge of) this node.
        let mut cursor = 0usize;
        let push_piece = |from: usize, to: usize, rebuilt: &mut Vec<InlineNode>| {
            if from < to {
                let mut piece = node.clone();
                if from != 0 || to != chars.len() {
                    piece.id = NodeId::new(format!("{}_c{from}", node.id.0));
                }
                piece.text = chars[from..to].iter().collect();
                rebuilt.push(InlineNode::Text(piece));
            }
        };

        // Start marker falls in this node when node_start <= plan.start <
        // node_end. The end marker falls in this node when node_start <
        // plan.end <= node_end. (A span boundary exactly at a node edge is
        // attributed to the node it opens/closes against.)
        let start_here = plan.start >= node_start && plan.start < node_end;

        // Determine the in-node cut points for markers.
        let mut cuts: Vec<(usize, &'static str)> = Vec::new();
        if start_here {
            cuts.push((local_start.min(chars.len()), "start"));
        }
        let end_here = plan.end > node_start && plan.end <= node_end;
        if end_here {
            cuts.push((local_end.min(chars.len()), "end_ref"));
        }
        cuts.sort_by_key(|(at, _)| *at);

        for (at, kind) in cuts {
            push_piece(cursor, at, &mut rebuilt);
            cursor = at;
            match kind {
                "start" => {
                    rebuilt.push(InlineNode::CommentRangeStart {
                        id: comment_id.to_string(),
                    });
                }
                "end_ref" => {
                    rebuilt.push(InlineNode::CommentRangeEnd {
                        id: comment_id.to_string(),
                    });
                    rebuilt.push(InlineNode::CommentReference {
                        id: comment_id.to_string(),
                    });
                }
                _ => unreachable!(),
            }
        }
        push_piece(cursor, chars.len(), &mut rebuilt);
    }

    // Splice the rebuilt run back where the original run was.
    let tail = inlines.split_off(plan.first);
    inlines.extend(rebuilt);
    inlines.extend(tail);
}

// ─── cross-segment anchor resolution (commenting a redlined paragraph) ────────

/// Where one comment-range marker lands, expressed against the paragraph's
/// segment list. `Split` cuts a Normal segment's text; `BeforeSeg` / `AfterSeg`
/// place the marker in its own zero-width Normal decoration segment on the edge
/// of a tracked (Inserted) segment — the widening that encloses a whole `w:ins`
/// rather than splitting it mid-run.
#[derive(Clone, Copy, Debug)]
enum Placement {
    /// Split the Normal segment `seg_idx` at char offset `off` (over that
    /// segment's text) and drop the marker at the cut.
    Split { seg_idx: usize, off: usize },
    /// Drop the marker just before segment `seg_idx` (its left boundary).
    BeforeSeg { seg_idx: usize },
    /// Drop the marker just after segment `seg_idx` (its right boundary).
    AfterSeg { seg_idx: usize },
}

/// The resolved placements for a cross-segment comment range.
struct CrossPlan {
    start: Placement,
    end: Placement,
}

/// True when a segment's text is part of the VISIBLE anchor space (present in
/// the going-forward document). Deleted and inserted-then-deleted text is struck
/// and excluded — an anchor may never resolve against it.
fn segment_is_visible(status: &TrackingStatus) -> bool {
    matches!(status, TrackingStatus::Normal | TrackingStatus::Inserted(_))
}

/// Resolve `expect` against the paragraph's visible text and decide where the
/// two range markers land. Refuses (never guesses) when the anchor cannot be
/// located, or when it falls on deleted content.
fn resolve_cross_segment_anchor(
    segments: &[TrackedSegment],
    expect: &str,
    block_id: &NodeId,
    step_index: usize,
) -> Result<CrossPlan, EditError> {
    // Visible text + a per-visible-char map to (segment index, char offset within
    // that segment's text). Text char offsets only — non-text inlines never
    // advance the offset, matching `splice_markers_in_segment`.
    let mut visible = String::new();
    let mut map: Vec<(usize, usize)> = Vec::new();
    for (si, seg) in segments.iter().enumerate() {
        if !segment_is_visible(&seg.status) {
            continue;
        }
        let mut local = 0usize;
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                for ch in t.text.chars() {
                    visible.push(ch);
                    map.push((si, local));
                    local += 1;
                }
            }
        }
    }

    let Some((start_k, end_k)) = char_find(&visible, expect) else {
        // Not in the visible text. If it IS present once deleted text is folded
        // back in, the caller is trying to comment on struck content — say so
        // with an actionable alternative rather than a bare "not found".
        let full: String = segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        if full.contains(expect) {
            return Err(EditError::CommentAnchorOverlapsDeleted {
                block_id: block_id.clone(),
                expected: expect.to_string(),
                step_index,
            });
        }
        return Err(EditError::CommentAnchorNotFound {
            block_id: block_id.clone(),
            expected: expect.to_string(),
            actual_text: visible,
            step_index,
        });
    };

    // `char_find` yields a non-empty match (expect is non-empty), so end_k >= 1.
    let (start_seg, start_local) = map[start_k];
    let (end_seg, end_local_last) = map[end_k - 1];
    // The end boundary is AFTER the last matched char.
    let end_off = end_local_last + 1;

    let start = match &segments[start_seg].status {
        // Normal: split the text at the boundary. Inserted (or anything else
        // reachable in the visible map, i.e. Inserted): widen left, enclosing the
        // whole container rather than splitting it mid-run.
        TrackingStatus::Normal => Placement::Split {
            seg_idx: start_seg,
            off: start_local,
        },
        _ => Placement::BeforeSeg { seg_idx: start_seg },
    };
    let end = match &segments[end_seg].status {
        TrackingStatus::Normal => Placement::Split {
            seg_idx: end_seg,
            off: end_off,
        },
        _ => Placement::AfterSeg { seg_idx: end_seg },
    };

    Ok(CrossPlan { start, end })
}

/// A zero-width Normal decoration segment carrying just marker inlines.
fn marker_segment(inlines: Vec<InlineNode>) -> TrackedSegment {
    TrackedSegment {
        status: TrackingStatus::Normal,
        inlines,
    }
}

/// Apply a resolved [`CrossPlan`]: rebuild the segment list, splitting Normal
/// segments at their cut offsets and inserting marker-only Normal segments on
/// the edges of tracked containers. Deleted / other segments between the
/// endpoints are passed through untouched — the range legally encloses them.
fn apply_cross_segment_markers(
    segments: &mut Vec<TrackedSegment>,
    plan: CrossPlan,
    comment_id: &str,
) {
    let start_before = match plan.start {
        Placement::BeforeSeg { seg_idx } => Some(seg_idx),
        _ => None,
    };
    let end_after = match plan.end {
        Placement::AfterSeg { seg_idx } => Some(seg_idx),
        _ => None,
    };
    let start_split = match plan.start {
        Placement::Split { seg_idx, off } => Some((seg_idx, off)),
        _ => None,
    };
    let end_split = match plan.end {
        Placement::Split { seg_idx, off } => Some((seg_idx, off)),
        _ => None,
    };

    let mut out: Vec<TrackedSegment> = Vec::with_capacity(segments.len() + 2);
    for (i, mut seg) in segments.drain(..).enumerate() {
        if start_before == Some(i) {
            out.push(marker_segment(vec![InlineNode::CommentRangeStart {
                id: comment_id.to_string(),
            }]));
        }
        let s_off = start_split.and_then(|(si, off)| (si == i).then_some(off));
        let e_off = end_split.and_then(|(si, off)| (si == i).then_some(off));
        if s_off.is_some() || e_off.is_some() {
            splice_markers_in_segment(&mut seg, s_off, e_off, comment_id);
        }
        out.push(seg);
        if end_after == Some(i) {
            out.push(marker_segment(vec![
                InlineNode::CommentRangeEnd {
                    id: comment_id.to_string(),
                },
                InlineNode::CommentReference {
                    id: comment_id.to_string(),
                },
            ]));
        }
    }
    *segments = out;
}

/// Split a single segment's text at up to two char offsets (`start_off` opens
/// the range with `commentRangeStart`; `end_off` closes it with
/// `commentRangeEnd` + `commentReference`) and splice the markers in. Offsets
/// count Text chars only; non-text inlines pass through unchanged. A boundary
/// exactly at a text-node edge attaches to the adjacent node (position is
/// identical either way).
fn splice_markers_in_segment(
    seg: &mut TrackedSegment,
    start_off: Option<usize>,
    end_off: Option<usize>,
    comment_id: &str,
) {
    // Cut list sorted by offset; at an equal offset the start marker precedes the
    // end marker so a zero-width range would still nest correctly.
    let mut cuts: Vec<(usize, u8)> = Vec::new();
    if let Some(s) = start_off {
        cuts.push((s, 0));
    }
    if let Some(e) = end_off {
        cuts.push((e, 1));
    }
    cuts.sort_by_key(|&(off, kind)| (off, kind));

    let push_marker = |kind: u8, out: &mut Vec<InlineNode>| match kind {
        0 => out.push(InlineNode::CommentRangeStart {
            id: comment_id.to_string(),
        }),
        _ => {
            out.push(InlineNode::CommentRangeEnd {
                id: comment_id.to_string(),
            });
            out.push(InlineNode::CommentReference {
                id: comment_id.to_string(),
            });
        }
    };

    let old = std::mem::take(&mut seg.inlines);
    let mut out: Vec<InlineNode> = Vec::with_capacity(old.len() + 3);
    let mut offset = 0usize; // text chars consumed so far
    let mut ci = 0usize; // next cut to place
    for inline in old {
        let InlineNode::Text(node) = inline else {
            out.push(inline);
            continue;
        };
        let chars: Vec<char> = node.text.chars().collect();
        let node_start = offset;
        let node_end = offset + chars.len();
        let mut cursor = 0usize; // local chars emitted from this node
        while ci < cuts.len() {
            let (cut_off, kind) = cuts[ci];
            if cut_off < node_start || cut_off > node_end {
                break;
            }
            let local = cut_off - node_start;
            if local > cursor {
                let mut piece = node.clone();
                if cursor != 0 || local != chars.len() {
                    piece.id = NodeId::new(format!("{}_c{cursor}", node.id.0));
                }
                piece.text = chars[cursor..local].iter().collect();
                out.push(InlineNode::Text(piece));
            }
            cursor = local;
            push_marker(kind, &mut out);
            ci += 1;
        }
        // Emit the node's tail (or the whole node when it carried no cut).
        if cursor < chars.len() {
            let mut piece = node.clone();
            if cursor != 0 {
                piece.id = NodeId::new(format!("{}_c{cursor}", node.id.0));
            }
            piece.text = chars[cursor..].iter().collect();
            out.push(InlineNode::Text(piece));
        } else if chars.is_empty() {
            out.push(InlineNode::Text(node));
        }
        offset = node_end;
    }
    // Cuts at the very end (no trailing text node to attach to) land here.
    while ci < cuts.len() {
        push_marker(cuts[ci].1, &mut out);
        ci += 1;
    }
    seg.inlines = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TextNode;

    fn text(id: &str, t: &str) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(id),
            text_role: None,
            text: t.to_string(),
            marks: vec![],
            style_props: Default::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            formatting_change: None,
        })
    }

    fn one_segment(inlines: Vec<InlineNode>) -> Vec<TrackedSegment> {
        vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines,
        }]
    }

    #[test]
    fn splice_wraps_exact_span_with_three_markers() {
        // "Hello world" → comment on "world".
        let mut segs = one_segment(vec![text("r0", "Hello world")]);
        let plan = find_span(&segs, "world").expect("found span");
        splice_markers(&mut segs, plan, "7");

        let inlines = &segs[0].inlines;
        // Expect: Text("Hello "), RangeStart, Text("world"), RangeEnd, Reference
        let kinds: Vec<&str> = inlines
            .iter()
            .map(|i| match i {
                InlineNode::Text(t) if t.text == "Hello " => "pre",
                InlineNode::Text(t) if t.text == "world" => "span",
                InlineNode::CommentRangeStart { .. } => "start",
                InlineNode::CommentRangeEnd { .. } => "end",
                InlineNode::CommentReference { .. } => "ref",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["pre", "start", "span", "end", "ref"]);
    }

    #[test]
    fn splice_span_in_middle_splits_both_sides() {
        // Comment on "lo wo" inside "Hello world".
        let mut segs = one_segment(vec![text("r0", "Hello world")]);
        let plan = find_span(&segs, "lo wo").expect("found span");
        splice_markers(&mut segs, plan, "1");

        // Reconstructed visible text must be unchanged.
        let visible: String = segs[0]
            .inlines
            .iter()
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(visible, "Hello world");

        // Marker ordering: start before the span, end+ref after it.
        let positions: Vec<&str> = segs[0]
            .inlines
            .iter()
            .filter_map(|i| match i {
                InlineNode::CommentRangeStart { .. } => Some("start"),
                InlineNode::CommentRangeEnd { .. } => Some("end"),
                InlineNode::CommentReference { .. } => Some("ref"),
                _ => None,
            })
            .collect();
        assert_eq!(positions, vec!["start", "end", "ref"]);
    }

    fn empty_doc() -> CanonDoc {
        CanonDoc {
            id: NodeId::from("doc"),
            blocks: vec![],
            meta: crate::domain::DocMeta {
                schema_version: crate::domain::SCHEMA_VERSION_V0.to_string(),
                docx_fingerprint: crate::domain::DocFingerprint("test".to_string()),
                internal_ids_version: crate::domain::INTERNAL_IDS_VERSION_V0.to_string(),
            },
            headers: vec![],
            footers: vec![],
            footnotes: vec![],
            endnotes: vec![],
            comments: vec![],
            comments_extended: vec![],
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: crate::domain::CompatSettings::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        }
    }

    #[test]
    fn fresh_comment_id_picks_max_plus_one() {
        let mut doc = empty_doc();
        assert_eq!(fresh_comment_id(&doc), "0");
        doc.comments.push(CommentStory {
            id: "4".to_string(),
            author: None,
            date: None,
            blocks: vec![],
            content_hash: String::new(),
            tracking_status: None,
        });
        assert_eq!(fresh_comment_id(&doc), "5");
    }
}
