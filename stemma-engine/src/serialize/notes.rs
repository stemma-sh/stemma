//! Story-part serialization (footnotes, endnotes, comments).
//!
//! Carved verbatim out of `serialize.rs` (now `serialize/mod.rs`). These
//! functions sync the note-like story collections back into their template
//! XML parts. Re-exported `pub(crate)` from the parent module so existing
//! `crate::serialize::serialize_*_part` call sites resolve unchanged.

use std::collections::HashSet;

use xmltree::{Element, XMLNode};

use super::{BookmarkIdPolicy, BookmarkScan, serialize_tracked_blocks};
use crate::docx::DocxArchive;
use crate::docx_package::DocxPackage;
use crate::domain::{CommentExtended, CommentStory, EndnoteStory, FootnoteStory, NoteType};
use crate::runtime::{
    COMMENTS_EXTENDED_REL_TYPE, COMMENTS_REL_TYPE, DocumentRelationships, ENDNOTES_REL_TYPE,
    FOOTNOTES_REL_TYPE, RuntimeError, invalid_docx, load_story_template_root, local_element_name,
    map_word_xml_error, relationship_target_to_part_path,
};
use crate::word_xml::{self, w_el};
use crate::xml_attrs::{attr_get, attr_set};

pub(crate) fn serialize_footnotes_part(
    base_pkg: &mut DocxPackage,
    target_archive: &DocxArchive,
    base_rels: &DocumentRelationships,
    target_rels: &DocumentRelationships,
    footnotes: &[FootnoteStory],
    next_id: &mut u32,
) -> Result<(), RuntimeError> {
    // When the document already references a footnotes part, reuse it. When it
    // does NOT (we are authoring the FIRST footnote into a doc that never had a
    // footnotes part), synthesize a fresh `word/footnotes.xml` carrying the
    // reserved separator + continuationSeparator notes (§17.11.10) — footnotes
    // are now an authorable construct, so the serializer must create the part
    // from scratch (mirrors `serialize_comments_part`), not fail.
    let existing_rel = base_rels
        .footnotes
        .as_ref()
        .or(target_rels.footnotes.as_ref());
    if footnotes.is_empty() && existing_rel.is_none() {
        // Nothing to author AND no existing part to reconcile — a genuine
        // no-op (the common "this document has never had footnotes" case).
        return Ok(());
    }
    // NOTE: `footnotes.is_empty()` with `existing_rel.is_some()` is NOT a
    // no-op — it means every authored note was removed this resolution (the
    // last DeleteNote accepted, or the only InsertNote rejected). The base/
    // target archives can still carry a PREVIOUSLY SERIALIZED footnotes.xml
    // with that note's content (`base_bytes`/`target_bytes` are re-zips of an
    // earlier snapshot — see `EditSnapshot::apply`/`project`), so skipping the
    // reconciliation below would silently leave that stale, now-orphaned
    // `<w:footnote>` element in the output — the exact silent-fallback
    // CLAUDE.md forbids. Falling through to `sync_note_like_part` (with an
    // empty `ordered_ids`) drops every non-reserved note and keeps only the
    // separator/continuationSeparator elements, which is the correct emptied
    // shape.
    //
    // Bookmark/move-range id policy for THIS part (ids pair per part —
    // §17.13.2): scan every note's blocks before any of them serialize.
    let mut bookmark_scan = BookmarkScan::default();
    for story in footnotes {
        bookmark_scan.scan_tracked_blocks(&story.blocks);
    }
    let bookmark_policy = &bookmark_scan.into_policy(next_id);
    let synthesized = existing_rel.is_none();
    let part_path = match existing_rel {
        Some(rel) => relationship_target_to_part_path(&rel.target),
        None => "word/footnotes.xml".to_string(),
    };
    let mut root = if synthesized {
        new_note_root("footnotes")
    } else {
        load_story_template_root(base_pkg, target_archive, &part_path)?
    };
    sync_note_like_part(
        &mut root,
        "footnote",
        footnotes.iter().map(|f| f.id.clone()).collect(),
        |id| {
            footnotes
                .iter()
                .find(|f| f.id == id)
                .map(|f| f.blocks.clone())
                .ok_or_else(|| {
                    invalid_docx(&format!(
                        "internal error: missing footnote story for id {id} during serialization"
                    ))
                })
        },
        |id, note| {
            attr_set(note, "w:id", id);
            let story = footnotes.iter().find(|f| f.id == id).ok_or_else(|| {
                invalid_docx(&format!(
                    "internal error: missing footnote metadata for id {id}"
                ))
            })?;
            let note_type = match story.note_type {
                NoteType::Normal => None,
                NoteType::Separator => Some("separator"),
                NoteType::ContinuationSeparator => Some("continuationSeparator"),
                NoteType::ContinuationNotice => Some("continuationNotice"),
            };
            if let Some(note_type) = note_type {
                attr_set(note, "w:type", note_type);
            }
            Ok(())
        },
        next_id,
        bookmark_policy,
    )?;
    word_xml::ensure_all_used_namespaces(&mut root);
    let xml = word_xml::write_document_xml(&root).map_err(map_word_xml_error)?;
    base_pkg.set_part(&part_path, xml);
    let ct_path = format!("/{part_path}");
    base_pkg.content_types.add_override(
        &ct_path,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
    );
    if synthesized {
        // Brand-new footnotes part: register the relationship (idempotent by
        // type+target) so document.xml.rels references it.
        base_pkg
            .document_rels
            .add(FOOTNOTES_REL_TYPE, "footnotes.xml");
    } else if base_rels.footnotes.is_none()
        && let Some(target_rel) = target_rels.footnotes.as_ref()
    {
        base_pkg.document_rels.add_with_preferred_id(
            FOOTNOTES_REL_TYPE,
            &target_rel.target,
            &target_rel.id,
        );
    }
    Ok(())
}

pub(crate) fn serialize_endnotes_part(
    base_pkg: &mut DocxPackage,
    target_archive: &DocxArchive,
    base_rels: &DocumentRelationships,
    target_rels: &DocumentRelationships,
    endnotes: &[EndnoteStory],
    next_id: &mut u32,
) -> Result<(), RuntimeError> {
    // First-endnote bootstrap: synthesize `word/endnotes.xml` with the reserved
    // separator notes when no endnotes part exists yet (mirrors footnotes /
    // comments). §17.11.2.
    let existing_rel = base_rels
        .endnotes
        .as_ref()
        .or(target_rels.endnotes.as_ref());
    if endnotes.is_empty() && existing_rel.is_none() {
        // See serialize_footnotes_part's identical guard for why this is NOT
        // simply `if endnotes.is_empty()`: an emptied-but-previously-serialized
        // part must still be reconciled down, not left stale.
        return Ok(());
    }
    // Per-part bookmark/move-range id policy (see serialize_footnotes_part).
    let mut bookmark_scan = BookmarkScan::default();
    for story in endnotes {
        bookmark_scan.scan_tracked_blocks(&story.blocks);
    }
    let bookmark_policy = &bookmark_scan.into_policy(next_id);
    let synthesized = existing_rel.is_none();
    let part_path = match existing_rel {
        Some(rel) => relationship_target_to_part_path(&rel.target),
        None => "word/endnotes.xml".to_string(),
    };
    let mut root = if synthesized {
        new_note_root("endnotes")
    } else {
        load_story_template_root(base_pkg, target_archive, &part_path)?
    };
    sync_note_like_part(
        &mut root,
        "endnote",
        endnotes.iter().map(|e| e.id.clone()).collect(),
        |id| {
            endnotes
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.blocks.clone())
                .ok_or_else(|| {
                    invalid_docx(&format!(
                        "internal error: missing endnote story for id {id} during serialization"
                    ))
                })
        },
        |id, note| {
            attr_set(note, "w:id", id);
            let story = endnotes.iter().find(|e| e.id == id).ok_or_else(|| {
                invalid_docx(&format!(
                    "internal error: missing endnote metadata for id {id}"
                ))
            })?;
            let note_type = match story.note_type {
                NoteType::Normal => None,
                NoteType::Separator => Some("separator"),
                NoteType::ContinuationSeparator => Some("continuationSeparator"),
                NoteType::ContinuationNotice => Some("continuationNotice"),
            };
            if let Some(note_type) = note_type {
                attr_set(note, "w:type", note_type);
            }
            Ok(())
        },
        next_id,
        bookmark_policy,
    )?;
    word_xml::ensure_all_used_namespaces(&mut root);
    let xml = word_xml::write_document_xml(&root).map_err(map_word_xml_error)?;
    base_pkg.set_part(&part_path, xml);
    let ct_path = format!("/{part_path}");
    base_pkg.content_types.add_override(
        &ct_path,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml",
    );
    if synthesized {
        base_pkg
            .document_rels
            .add(ENDNOTES_REL_TYPE, "endnotes.xml");
    } else if base_rels.endnotes.is_none()
        && let Some(target_rel) = target_rels.endnotes.as_ref()
    {
        base_pkg.document_rels.add_with_preferred_id(
            ENDNOTES_REL_TYPE,
            &target_rel.target,
            &target_rel.id,
        );
    }
    Ok(())
}

pub(crate) fn serialize_comments_part(
    base_pkg: &mut DocxPackage,
    target_archive: &DocxArchive,
    base_rels: &DocumentRelationships,
    target_rels: &DocumentRelationships,
    comments: &[CommentStory],
    next_id: &mut u32,
) -> Result<(), RuntimeError> {
    // Per-part bookmark/move-range id policy (see serialize_footnotes_part).
    let mut bookmark_scan = BookmarkScan::default();
    for story in comments {
        bookmark_scan.scan_tracked_blocks(&story.blocks);
    }
    let bookmark_policy = &bookmark_scan.into_policy(next_id);
    // When the document already references a comments part (in base or target),
    // reuse that path + template. When it does NOT (we are authoring the first
    // comment into a doc that never had one), synthesize a fresh
    // `word/comments.xml` with an empty `w:comments` root — comments are now an
    // authorable construct, so the serializer must be able to create the part
    // from scratch (the people.xml pattern), not fail.
    let existing_rel = base_rels
        .comments
        .as_ref()
        .or(target_rels.comments.as_ref());
    // With no comments in the IR there are two cases. If the document never had a
    // comments part (`existing_rel` is None), there is nothing to write — return.
    // But if a comments part DOES exist (e.g. every comment was deleted), it still
    // carries the now-orphaned `<w:comment>` definitions; we must fall through and
    // reconcile it down to an empty `<w:comments/>` root, or Word raises a repair
    // dialog over the dangling definition (the validator's I-XREF-003 only catches
    // dangling *references*, not orphaned *definitions*).
    if comments.is_empty() && existing_rel.is_none() {
        return Ok(());
    }
    let synthesized = existing_rel.is_none();
    let part_path = match existing_rel {
        Some(rel) => relationship_target_to_part_path(&rel.target),
        None => "word/comments.xml".to_string(),
    };
    let mut root = if synthesized {
        new_comments_root()
    } else {
        load_story_template_root(base_pkg, target_archive, &part_path)?
    };
    sync_note_like_part(
        &mut root,
        "comment",
        comments.iter().map(|c| c.id.clone()).collect(),
        |id| {
            comments
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.blocks.clone())
                .ok_or_else(|| {
                    invalid_docx(&format!(
                        "internal error: missing comment story for id {id} during serialization"
                    ))
                })
        },
        |id, note| {
            attr_set(note, "w:id", id);
            if let Some(story) = comments.iter().find(|c| c.id == id) {
                if let Some(author) = &story.author {
                    attr_set(note, "w:author", author.clone());
                }
                if let Some(date) = &story.date {
                    attr_set(note, "w:date", date.clone());
                }
            }
            Ok(())
        },
        next_id,
        bookmark_policy,
    )?;
    word_xml::ensure_all_used_namespaces(&mut root);
    let xml = word_xml::write_document_xml(&root).map_err(map_word_xml_error)?;
    base_pkg.set_part(&part_path, xml);
    let ct_path = format!("/{part_path}");
    base_pkg.content_types.add_override(
        &ct_path,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
    );
    if synthesized {
        // Brand-new comments part: register the relationship (idempotent by
        // type+target) so document.xml.rels references it.
        base_pkg
            .document_rels
            .add(COMMENTS_REL_TYPE, "comments.xml");
    } else if base_rels.comments.is_none()
        && let Some(target_rel) = target_rels.comments.as_ref()
    {
        base_pkg.document_rels.add_with_preferred_id(
            COMMENTS_REL_TYPE,
            &target_rel.target,
            &target_rel.id,
        );
    }
    Ok(())
}

/// A fresh `w:footnotes` / `w:endnotes` root for a synthesized note part (a
/// document that had no footnotes.xml / endnotes.xml before the first authored
/// note). Pre-populates the two **reserved** notes Word requires at the top of
/// every note part (§17.11.10 / §17.11.2): the `separator` (id `-1`) and
/// `continuationSeparator` (id `0`) notes, each a single paragraph whose run
/// holds the `w:separator` / `w:continuationSeparator` placeholder. These ids
/// are reserved and the note-id allocator never reuses them.
///
/// `note_tag` is `"footnotes"` or `"endnotes"`; the per-note element tag
/// (`footnote` / `endnote`) and the placeholder element name are derived from
/// it. `sync_note_like_part` then appends each authored note;
/// `ensure_all_used_namespaces` finalizes the namespace set.
fn new_note_root(note_tag: &str) -> Element {
    let singular = match note_tag {
        "footnotes" => "footnote",
        "endnotes" => "endnote",
        other => unreachable!("new_note_root called with unexpected tag {other}"),
    };
    let mut root = Element::new(note_tag);
    root.prefix = Some("w".to_string());
    let mut ns = xmltree::Namespace::empty();
    ns.put(
        "w",
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    );
    root.namespaces = Some(ns);

    for (id, note_type) in [("-1", "separator"), ("0", "continuationSeparator")] {
        let mut note = w_el(singular);
        attr_set(&mut note, "w:id", id);
        attr_set(&mut note, "w:type", note_type);
        // <w:p><w:r><w:separator/></w:r></w:p>
        let mut run = w_el("r");
        run.children.push(XMLNode::Element(w_el(note_type)));
        let mut para = w_el("p");
        para.children.push(XMLNode::Element(run));
        note.children.push(XMLNode::Element(para));
        root.children.push(XMLNode::Element(note));
    }
    root
}

/// A fresh, empty `w:comments` root for a synthesized comments part (a document
/// that had no comments.xml before the first authored comment). Declares the
/// common WordprocessingML namespaces; `sync_note_like_part` appends each
/// `w:comment` and `ensure_all_used_namespaces` finalizes the namespace set.
fn new_comments_root() -> Element {
    let mut root = Element::new("comments");
    root.prefix = Some("w".to_string());
    let mut ns = xmltree::Namespace::empty();
    ns.put(
        "w",
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    );
    ns.put(
        "w14",
        "http://schemas.microsoft.com/office/word/2010/wordml",
    );
    ns.put(
        "w15",
        "http://schemas.microsoft.com/office/word/2012/wordml",
    );
    root.namespaces = Some(ns);
    root
}

/// Emit `word/commentsExtended.xml` (MS-DOCX §2.5.1) from the typed
/// [`CommentExtended`] model, plus its relationship + content-type override.
///
/// Modeled on `build_people_xml` (a flat list synthesized from the IR, not a
/// template sync): every `w15:commentEx` carries `w15:paraId`, an optional
/// `w15:paraIdParent` (reply threading), and `w15:done`. This replaces the
/// previous opaque byte-passthrough — a document we never author into still
/// round-trips equivalently (same paraId / parent / done set), just not
/// necessarily byte-for-byte.
///
/// Each authored comment's first body paragraph must already carry the
/// `w14:paraId` referenced here (set by the comments verb at authoring time);
/// the paraIds are not allocated here, so there is no collision with the
/// serializer's annotation-id allocator.
pub(crate) fn serialize_comments_extended_part(
    base_pkg: &mut DocxPackage,
    records: &[CommentExtended],
) -> Result<(), RuntimeError> {
    if records.is_empty() {
        return Ok(());
    }
    const PART_PATH: &str = "word/commentsExtended.xml";

    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#);
    xml.push_str(
        r#"<w15:commentsEx xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup" xmlns:wpi="http://schemas.microsoft.com/office/word/2010/wordprocessingInk" xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" mc:Ignorable="w14 w15 wp14">"#,
    );
    for rec in records {
        xml.push_str(&format!(
            r#"<w15:commentEx w15:paraId="{}""#,
            xml_attr_escape(&rec.para_id)
        ));
        if let Some(parent) = &rec.para_id_parent {
            xml.push_str(&format!(
                r#" w15:paraIdParent="{}""#,
                xml_attr_escape(parent)
            ));
        }
        xml.push_str(&format!(
            r#" w15:done="{}"/>"#,
            if rec.done { "1" } else { "0" }
        ));
    }
    xml.push_str("</w15:commentsEx>");

    base_pkg.set_part(PART_PATH, xml.into_bytes());
    base_pkg.content_types.add_override(
        "/word/commentsExtended.xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml",
    );
    // Idempotent: add the relationship only if the package doesn't already
    // reference the part (a document that already had commentsExtended keeps
    // its original rId via the passthrough rels).
    base_pkg
        .document_rels
        .add(COMMENTS_EXTENDED_REL_TYPE, "commentsExtended.xml");
    Ok(())
}

/// Reconcile `word/commentsIds.xml` (the `w16cid` durable-id sidecar, MS-DOCX
/// §2.5.3.1) against the current comment set — but ONLY when the package
/// already carries the part.
///
/// The part lists one `w16cid:commentId` per comment, keyed on the comment's
/// last-body-paragraph `w14:paraId` (the same key `commentsExtended` and reply
/// threading use) plus a durable 8-hex `w16cid:durableId`. It is opaque
/// passthrough on import, so a newly-authored comment's paraId is missing from
/// it; Word then distrusts the new comment's extended metadata (the reply shows
/// unthreaded, thread resolve reads `done=false`) even though comments.xml and
/// commentsExtended.xml are correct.
///
/// This re-emits the part from `comments`: every current comment gets an entry
/// (existing durableIds preserved by paraId — they are Word's stable comment
/// identities; a fresh unique durableId minted for a newly-authored comment),
/// and a comment deleted since import drops out (no dangling durableIds),
/// consistent with the whole-thread delete contract. Word does not require the
/// part, so we NEVER create it where it was absent — a document without it is
/// left untouched.
pub(crate) fn serialize_comments_ids_part(
    base_pkg: &mut DocxPackage,
    comments: &[CommentStory],
) -> Result<(), RuntimeError> {
    const PART_PATH: &str = "word/commentsIds.xml";
    let Some(existing_bytes) = base_pkg.get_part(PART_PATH) else {
        return Ok(());
    };

    // Parse existing paraId -> durableId, and the set of durableIds in use (so
    // a freshly minted id cannot collide with a surviving one).
    let root = word_xml::parse_document_xml(existing_bytes).map_err(map_word_xml_error)?;
    let mut existing: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut used: HashSet<String> = HashSet::new();
    for child in &root.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        if local_element_name(el) != "commentId" {
            continue;
        }
        let (Some(para_id), Some(durable)) = (
            attr_get(el, "w16cid:paraId").or_else(|| attr_get(el, "paraId")),
            attr_get(el, "w16cid:durableId").or_else(|| attr_get(el, "durableId")),
        ) else {
            continue;
        };
        used.insert(durable.clone());
        existing
            .entry(para_id.clone())
            .or_insert_with(|| durable.clone());
    }

    // One entry per current comment, in document order, keyed on the comment's
    // LAST paragraph. A comment with no paraId cannot be keyed, so it is skipped
    // (it would carry no commentsExtended record either).
    let mut entries: Vec<(String, String)> = Vec::with_capacity(comments.len());
    for c in comments {
        let Some(para_id) = c.last_para_id() else {
            continue;
        };
        let durable = match existing.get(para_id) {
            Some(d) => d.clone(),
            None => {
                let d = fresh_durable_id(para_id, &used);
                used.insert(d.clone());
                d
            }
        };
        entries.push((para_id.to_string(), durable));
    }

    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#);
    xml.push_str(
        r#"<w16cid:commentsIds xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w16cid="http://schemas.microsoft.com/office/word/2016/wordml/cid" mc:Ignorable="w16cid">"#,
    );
    for (para_id, durable) in &entries {
        xml.push_str(&format!(
            r#"<w16cid:commentId w16cid:paraId="{}" w16cid:durableId="{}"/>"#,
            xml_attr_escape(para_id),
            xml_attr_escape(durable)
        ));
    }
    xml.push_str("</w16cid:commentsIds>");

    // The part (with its content-type override + relationship) already exists
    // via passthrough; we only rewrite its bytes.
    base_pkg.set_part(PART_PATH, xml.into_bytes());
    Ok(())
}

/// Mint a fresh 8-hex `w16cid:durableId` (uppercase, matching Word's format)
/// not colliding with `used`. Seeded deterministically from the comment's paraId
/// so a single serialize is reproducible; bumps on the (vanishingly unlikely)
/// collision. Mirrors the comment verb's `fresh_para_id`.
fn fresh_durable_id(para_id: &str, used: &HashSet<String>) -> String {
    let mut seed: u32 = 0x1000_0000;
    for b in para_id.bytes() {
        seed = seed.wrapping_mul(31).wrapping_add(b as u32);
    }
    loop {
        let candidate = format!("{seed:08X}");
        if !used.contains(&candidate) {
            return candidate;
        }
        seed = seed.wrapping_add(1);
    }
}

/// Escape XML attribute special characters (matches `build_people_xml`).
fn xml_attr_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Is this note element one of the reserved footnote/endnote notes (§17.11.10)?
/// The `separator` and `continuationSeparator` notes (conventionally ids `-1`
/// and `0`) are structural placeholders, not user content: they never enter the
/// IR's footnote/endnote collection, so `sync_note_like_part` must keep them even
/// though their id is absent from the authored id set. Comments have no reserved
/// notes, so a `w:comment` element never matches here.
fn is_reserved_note(note: &Element) -> bool {
    matches!(
        crate::xml_attrs::attr_get(note, "w:type")
            .or_else(|| crate::xml_attrs::attr_get(note, "type"))
            .map(String::as_str),
        Some("separator") | Some("continuationSeparator")
    )
}

pub(crate) fn sync_note_like_part(
    root: &mut Element,
    note_tag: &str,
    ordered_ids: Vec<String>,
    blocks_for_id: impl Fn(&str) -> Result<Vec<crate::domain::TrackedBlock>, RuntimeError>,
    configure_note_attrs: impl Fn(&str, &mut Element) -> Result<(), RuntimeError>,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
) -> Result<(), RuntimeError> {
    let ordered_id_set: HashSet<&str> = ordered_ids.iter().map(|s| s.as_str()).collect();
    let mut seen = HashSet::new();
    // Reconcile the template part against the live IR id set: UPDATE the body of
    // each note/comment whose id is still in the IR, and DROP any template
    // element whose id is absent (a deleted comment/note definition — leaving it
    // would orphan a `<w:comment>`/`<w:footnote>`/`<w:endnote>` and make Word
    // raise a repair dialog; the validator's I-XREF-003 only catches dangling
    // *references*, not orphaned *definitions*). Non-note children and notes
    // without a parseable id are left untouched. Reserved separator /
    // continuationSeparator notes (footnote/endnote ids -1 and 0) survive because
    // `apply_delete` retains them in the IR, so they remain in `ordered_id_set`.
    let mut retained = Vec::with_capacity(root.children.len());
    for child in root.children.drain(..) {
        let XMLNode::Element(mut note) = child else {
            retained.push(child);
            continue;
        };
        if local_element_name(&note) != note_tag {
            retained.push(XMLNode::Element(note));
            continue;
        }
        let Some(id) = crate::xml_attrs::attr_get(&note, "w:id")
            .or_else(|| crate::xml_attrs::attr_get(&note, "id"))
            .map(|v| v.to_string())
        else {
            retained.push(XMLNode::Element(note));
            continue;
        };
        if !ordered_id_set.contains(id.as_str()) {
            // Id not authored in the IR. Two sub-cases:
            //  - Reserved separator / continuationSeparator notes (§17.11.10):
            //    these are NOT user notes and never enter the IR's footnote /
            //    endnote collection, yet Word requires them at the top of every
            //    note part. Keep them verbatim.
            //  - Any other id: an orphaned definition (a deleted comment / note).
            //    Drop it, or Word raises a repair dialog over the dangling
            //    definition (I-XREF-003 only catches dangling references).
            if is_reserved_note(&note) {
                retained.push(XMLNode::Element(note));
            }
            continue;
        }
        let blocks = blocks_for_id(&id)?;
        note.children = serialize_tracked_blocks(&blocks, next_id, bookmark_policy, None)?;
        seen.insert(id);
        retained.push(XMLNode::Element(note));
    }
    root.children = retained;

    for id in ordered_ids {
        if seen.contains(&id) {
            continue;
        }
        let mut note = w_el(note_tag);
        configure_note_attrs(&id, &mut note)?;
        let blocks = blocks_for_id(&id)?;
        note.children = serialize_tracked_blocks(&blocks, next_id, bookmark_policy, None)?;
        root.children.push(XMLNode::Element(note));
    }

    Ok(())
}
