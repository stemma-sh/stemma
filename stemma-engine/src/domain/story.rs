//! Story-part domain types: headers, footers, footnotes, endnotes, comments.
//!
//! Carved verbatim out of `domain.rs` (now `domain/mod.rs`). These are the
//! block-bearing "stories" that live alongside the main document body. The
//! IR serde contract is identical to before the split — every `#[derive]`
//! and field is byte-for-byte the same.

use serde::{Deserialize, Serialize};

use super::{NodeId, TrackedBlock, TrackingStatus};

/// Header story from a header part (word/headerN.xml).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HeaderStory {
    /// Part name (e.g., "header1.xml") — the relationship target, used as
    /// the canonical identifier for this story across documents.
    pub part_name: String,
    /// Header type (default, first page, even pages).
    pub kind: HeaderFooterKind,
    /// Block content of the header.
    pub blocks: Vec<TrackedBlock>,
    /// Content hash for cross-document alignment.
    pub content_hash: String,
    /// §17.10.5 blank synthesis: this story exists only to model Word's
    /// render-time blank default header/footer for the first section — it was
    /// never authored and must not serialize (neither part nor reference).
    #[serde(default)]
    pub synthesized: bool,
}

/// Footer story from a footer part (word/footerN.xml).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FooterStory {
    /// Part name (e.g., "footer1.xml") — the relationship target, used as
    /// the canonical identifier for this story across documents.
    pub part_name: String,
    /// Footer type (default, first page, even pages).
    pub kind: HeaderFooterKind,
    /// Block content of the footer.
    pub blocks: Vec<TrackedBlock>,
    /// Content hash for cross-document alignment.
    pub content_hash: String,
    /// §17.10.5 blank synthesis: this story exists only to model Word's
    /// render-time blank default header/footer for the first section — it was
    /// never authored and must not serialize (neither part nor reference).
    #[serde(default)]
    pub synthesized: bool,
}

/// Type of header or footer (corresponds to w:type attribute).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HeaderFooterKind {
    /// Default header/footer for most pages.
    Default,
    /// Header/footer for first page only.
    First,
    /// Header/footer for even pages (when different odd/even enabled).
    Even,
}

impl HeaderFooterKind {
    /// The `w:type` attribute value (§17.10.1 ST_HdrFtr): the canonical
    /// serialization of this kind, also used as the `kind` tag projected to the
    /// frontend so it can pick the applicable band.
    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::First => "first",
            Self::Even => "even",
        }
    }
}

/// Footnote story from word/footnotes.xml.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FootnoteStory {
    /// Note ID for linking to references in body.
    pub id: String,
    /// Type of footnote (normal vs separator/continuation).
    pub note_type: NoteType,
    /// Block content of the footnote.
    pub blocks: Vec<TrackedBlock>,
    /// Content hash for cross-document alignment.
    pub content_hash: String,
}

/// Endnote story from word/endnotes.xml.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EndnoteStory {
    /// Note ID for linking to references in body.
    pub id: String,
    /// Type of endnote (normal vs separator/continuation).
    pub note_type: NoteType,
    /// Block content of the endnote.
    pub blocks: Vec<TrackedBlock>,
    /// Content hash for cross-document alignment.
    pub content_hash: String,
}

/// Type of footnote/endnote.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum NoteType {
    /// User content - include in diff.
    Normal,
    /// Separator line - exclude from diff.
    Separator,
    /// Continuation separator - exclude from diff.
    ContinuationSeparator,
    /// Continuation notice - exclude from diff.
    ContinuationNotice,
}

/// Comment story from word/comments.xml.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommentStory {
    /// Comment ID for linking to anchors in body.
    pub id: String,
    /// Author of the comment.
    pub author: Option<String>,
    /// Date/time of the comment.
    pub date: Option<String>,
    /// Block content of the comment.
    pub blocks: Vec<TrackedBlock>,
    /// Content hash for cross-document alignment.
    pub content_hash: String,
    /// Tracking status for the whole comment story (e.g. Deleted when the
    /// target document no longer has this comment). Used by accept_all /
    /// reject_all to remove or keep the story without marking individual
    /// blocks, which would cause w:del / w:delText in serialized XML.
    pub tracking_status: Option<TrackingStatus>,
}

/// A `w15:commentEx` record from word/commentsExtended.xml (MS-DOCX §2.5.1).
///
/// This is the threading/resolve sidecar for comments. It links by the
/// `w14:paraId` of a comment's **LAST body paragraph** (NOT the comment `w:id`,
/// and NOT the first paragraph — see the `para_id` field): `para_id` identifies
/// this comment, `para_id_parent` (when present) names the comment this one
/// replies to, and `done` is the resolved flag.
///
/// `done` is a **thread** property: Word derives a comment's resolved state
/// from the thread ROOT's record (the one with no `para_id_parent`). Resolving
/// therefore acts on the whole thread, not a single reply (see the resolve
/// verb).
///
/// This used to be opaque byte-passthrough; it is now a typed model so the
/// reply / resolve verbs can author and mutate it. Documents we never author
/// into still round-trip equivalently (same `paraId` / parent / done set),
/// just not necessarily byte-for-byte.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommentExtended {
    /// The `w14:paraId` of the comment's **last body paragraph** this record
    /// describes. MS-DOCX §2.5.1 keys `commentEx` on the last paragraph's
    /// paraId, not the first — for a single-paragraph comment they coincide, but
    /// for a multi-paragraph comment only the LAST paragraph's paraId resolves
    /// the record. The link back to a `CommentStory` is via
    /// [`CommentStory::last_para_id`] (the comment `w:id` is a separate
    /// identifier — the `commentsExtended` part keys on paraId, not comment id).
    pub para_id: String,
    /// The `w14:paraId` of the parent comment when this is a reply
    /// (`w15:paraIdParent`) — the parent's LAST-paragraph paraId, matching the
    /// keying above. `None` for a top-level (thread-root) comment.
    pub para_id_parent: Option<String>,
    /// The `w15:done` resolved flag. Meaningful on the thread root; see the
    /// type-level note.
    pub done: bool,
}

impl CommentStory {
    /// The `w14:paraId` of this comment's FIRST body paragraph. This is a plain
    /// positional accessor; it is **not** the `commentsExtended` key — that is
    /// the last paragraph's paraId (see [`CommentStory::last_para_id`]). For a
    /// single-paragraph comment the two coincide. `None` if the comment has no
    /// paragraph block or that paragraph carries no `w14:paraId`.
    pub fn first_para_id(&self) -> Option<&str> {
        self.blocks.iter().find_map(|b| match &b.block {
            super::BlockNode::Paragraph(p) => p.para_id.as_deref(),
            _ => None,
        })
    }

    /// The `w14:paraId` of this comment's LAST body paragraph — the key used by
    /// `commentsExtended` (MS-DOCX §2.5.1) to thread replies and record the
    /// resolved flag. MS-DOCX keys the record on the last paragraph, so this is
    /// the accessor every reply / resolve / delete join must use. `None` if the
    /// comment has no paragraph block, or its last paragraph carries no
    /// `w14:paraId`.
    pub fn last_para_id(&self) -> Option<&str> {
        self.blocks.iter().rev().find_map(|b| match &b.block {
            super::BlockNode::Paragraph(p) => p.para_id.as_deref(),
            _ => None,
        })
    }
}

/// Walk `para_id_parent` links up from `start_key` to the thread ROOT's key
/// (the record with no parent). Missing/dangling parents and cycles terminate
/// the walk at the last resolvable key. Returns `start_key` unchanged when it
/// has no record.
pub fn thread_root_key(comments_extended: &[CommentExtended], start_key: &str) -> String {
    let mut current = start_key.to_string();
    // The chain cannot be longer than the record count without a cycle; bound
    // the walk by that to guarantee termination.
    for _ in 0..=comments_extended.len() {
        match comments_extended.iter().find(|r| r.para_id == current) {
            Some(rec) => match &rec.para_id_parent {
                Some(parent) if comments_extended.iter().any(|r| &r.para_id == parent) => {
                    current = parent.clone();
                }
                _ => break,
            },
            None => break,
        }
    }
    current
}

/// Look up a comment's resolved/threaded state, returning
/// `(resolved, parent_para_id)`. Keys on the comment's LAST-paragraph paraId
/// (MS-DOCX §2.5.1); `resolved` is read from the comment's own record, and —
/// because `done` is a thread property Word derives from the root — from the
/// thread root's record when the two disagree. `parent_para_id` is the
/// comment's own reply link. Returns `(false, None)` when the comment has no
/// paraId or no matching record — the honest "no extended metadata" answer, not
/// a fabricated default.
pub fn comment_extended_state(
    comment: &CommentStory,
    comments_extended: &[CommentExtended],
) -> (bool, Option<String>) {
    let Some(para_id) = comment.last_para_id() else {
        return (false, None);
    };
    match comments_extended.iter().find(|r| r.para_id == para_id) {
        Some(rec) => {
            // Resolved is thread-derived: read the root's flag so an imported
            // thread whose root alone carries done=1 still reports resolved.
            let root_key = thread_root_key(comments_extended, para_id);
            let resolved = comments_extended
                .iter()
                .find(|r| r.para_id == root_key)
                .map(|r| r.done)
                .unwrap_or(rec.done);
            (resolved, rec.para_id_parent.clone())
        }
        None => (false, None),
    }
}

/// Typed story scope for NodeId (identifies which story a node belongs to).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum StoryScope {
    /// Main document body.
    Body,
    /// Header with given part path and kind.
    Header {
        part_path: String,
        kind: HeaderFooterKind,
    },
    /// Footer with given part path and kind.
    Footer {
        part_path: String,
        kind: HeaderFooterKind,
    },
    /// Footnote with given ID.
    Footnote { id: String },
    /// Endnote with given ID.
    Endnote { id: String },
    /// Comment with given ID.
    Comment { id: String },
    /// A textbox interior (Word's "text frame" story), identified by the hosting
    /// opaque drawing's `anchor` id. Interior tracked changes inside a textbox are
    /// attributed to this scope — NOT the body/header story that hosts the drawing
    /// — so a per-story revision consumer sees them instead of a false zero
    /// (RFC-0002 §Phase-3, per-story attribution). Appended last so bincode variant
    /// indices of the older scopes stay stable.
    TextFrame { anchor: NodeId },
}
