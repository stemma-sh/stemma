//! Header / footer authoring verbs (§17.10). "Edit the running head; show a
//! distinct first-page header; use different even/odd footers; link an existing
//! header to this section."
//!
//! Two kinds of operation live here:
//!
//! 1. **Content edits** (`EditHeader` / `EditFooter`) — a TRACKED text edit of a
//!    paragraph inside a header/footer story. These route through the SAME
//!    word-diff + ONE materializer that body `ReplaceParagraphText` uses
//!    (`super::super::apply_replace_paragraph_text` /
//!    `apply_segment_replace_paragraph` and `validate_preserved_inlines`),
//!    operating over the story resolved by `story_addr`. Invariant M holds: we
//!    add no second materializer — we call the body one over a story-scoped
//!    paragraph. Header/footer stories reuse the body `p_{n}` block-id
//!    namespace, so the paragraph is addressed via `(StoryRef::Header|Footer,
//!    NodeId)` — a bare NodeId is ambiguous across stories.
//!
//! 2. **Mode toggles** (`SetHeaderFooterMode`) — flip `w:titlePg` (§17.6.18) on
//!    a section, flip the document-level `w:evenAndOddHeaders` (§17.15.1.35),
//!    and link/unlink a section's `headerReference` / `footerReference` by kind.
//!
//! v1 scope (fail loud beyond it):
//! - LINK targets an EXISTING header/footer story (addressed by kind, resolved
//!   to a part name). Net-new-story creation (no header exists at all) is OUT of
//!   v1: we fail `HeaderFooterRefNotResolvable` rather than best-effort
//!   synthesizing an empty story.
//! - Editing existing header/footer content IS in scope.
//! - `EditHeader`/`EditFooter` target a top-level paragraph of the story that is
//!   Normal with no existing tracked segments (same gate as body replace); the
//!   opaque inventory (a PAGE field run, etc.) must be preserved or the edit
//!   fails `OpaqueDestroyed`.

use super::super::story_addr::{
    StoryRef, footer_part_for_kind, header_part_for_kind, story_blocks, story_blocks_mut,
};
use super::super::{
    EditError, MaterializationMode, ParagraphContent, apply_replace_paragraph_text,
    apply_segment_replace_paragraph, collect_anchor_inventory, expect_matches_paragraph,
    is_identity_replacement, next_revision, paragraph_visible_text, validate_block_is_editable,
    validate_preserved_inlines,
};
use crate::domain::{
    BlockNode, CanonDoc, FooterStory, HeaderFooterKind, HeaderStory, NodeId, ParagraphNode,
    RevisionInfo, SectionProperties, SectionPropertyChange, TrackedBlock, TrackingStatus,
    normal_tracked_block,
};
use crate::semantic_hash::check_block_guard;
use crate::tracked_model::project_block_for_accept_reject;

/// Which header/footer reference kind a `SetHeaderFooterMode` link/unlink names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderFooterLink {
    /// Whether the link is for a header (`headerReference`) or footer
    /// (`footerReference`).
    pub is_header: bool,
    /// The header/footer kind (`Default` / `First` / `Even`) — selects the
    /// `w:type` of the reference and the existing story to resolve.
    pub kind: HeaderFooterKind,
    /// `true` = link (add the reference); `false` = unlink (remove it).
    pub link: bool,
}

/// `EditHeader` / `EditFooter` — tracked text edit of a header/footer story
/// paragraph. `story` must be `StoryRef::Header`/`Footer`; the block is resolved
/// within that story (story-local id), then the body materializer runs over it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_edit(
    doc: &mut CanonDoc,
    story: &StoryRef,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    content: &ParagraphContent,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Guard: this verb addresses header/footer stories only. A Body/footnote/
    // comment story would be a programmer error in the dispatch arm.
    match story {
        StoryRef::Header(_) | StoryRef::Footer(_) => {}
        other => {
            return Err(EditError::StoryNotFound {
                story: other.clone(),
                step_index,
            });
        }
    }

    // Resolve the paragraph within the named story, at the top level or inside
    // a table of it (a letterhead's contact cell), never in the body: block ids
    // are story-local and the body has its own.
    let path = {
        let blocks = story_blocks(doc, story, step_index)?;
        let path = super::super::find_paragraph_path_in(blocks, block_id).ok_or_else(|| {
            EditError::StoryBlockNotFound {
                story: story.clone(),
                block_id: block_id.clone(),
                step_index,
            }
        })?;
        // Block-status + segment-Normal preconditions: a top-level paragraph
        // by its own status, a cell paragraph by the table, rows and cells
        // above it and by its own runs.
        if path.is_top_level() {
            validate_block_is_editable(&blocks[path.top_block], step_index)?;
        } else {
            super::super::check_ancestor_table_tracking_in(blocks, &path, block_id, step_index)?;
            if let BlockNode::Paragraph(p) = super::super::block_at_in(blocks, &path)
                && p.segments
                    .iter()
                    .any(|seg| seg.status != TrackingStatus::Normal)
            {
                return Err(EditError::ParagraphContainsTrackedSegments {
                    block_id: block_id.clone(),
                    step_index,
                });
            }
        }
        path
    };

    // Run the body materializer over the resolved story paragraph: the SAME
    // validation + lowering the body path uses, on a story-scoped block.
    let blocks = story_blocks_mut(doc, story, step_index)?;
    let block = super::super::block_at_mut_in(blocks, &path);

    match &*block {
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

    // Optional semantic-hash precondition (stale-snapshot detection).
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

    let BlockNode::Paragraph(para) = &*block else {
        unreachable!("checked paragraph above");
    };

    // `expect` matches the paragraph's read text, as for a body replace
    // (`expect_matches_paragraph`). An empty `expect` is not a substring that
    // matches anything: it asserts the paragraph is empty, which is the only way
    // to name an empty footer line before typing into it.
    let expect_found = if expect.trim().is_empty() {
        paragraph_visible_text(para).trim().is_empty()
    } else {
        expect_matches_paragraph(para, expect)
    };
    if !expect_found {
        return Err(EditError::ExpectMismatch {
            block_id: block_id.clone(),
            expected: expect.to_string(),
            actual_text: paragraph_visible_text(para),
            step_index,
        });
    }

    // `expect` names text in the paragraph and `content` what replaces it.
    // Content that is only the replacement (plain text repeating none of the
    // paragraph's text around `expect`: "March 2024 v3" for "November 2023
    // v2" beside a footer's page fields) replaces just that part, keeping the
    // fields and the rest. Content that rebuilds the paragraph (it repeats
    // the surrounding text, or carries formatting, links or kept objects)
    // replaces the whole paragraph, as when `expect` is its whole text.
    if let Some(replacement) = part_replacement(para, expect, content) {
        let occurrences = super::find_replace::paragraph_match_count(para, expect);
        if occurrences != 1 {
            return Err(EditError::UnsupportedParagraphStructure {
                block_id: block_id.clone(),
                reason: format!(
                    "expect {expect:?} occurs {occurrences} times in the paragraph as editable \
                     text; name a part that occurs once, or give the paragraph's whole text"
                ),
                step_index,
            });
        }
        let options = super::find_replace::ReplaceTextOptions {
            old: expect.to_string(),
            new: replacement,
            author: revision.author.clone().unwrap_or_default(),
            scope: super::find_replace::ReplaceTextScope::WholeDoc,
            expected: super::find_replace::ExpectedMatches::Count(1),
            match_mode: super::find_replace::MatchMode::Exact,
            on_barrier_match: super::find_replace::BarrierPolicy::Fail,
        };
        let (start, end, splice) =
            super::find_replace::plan_paragraph_part_splice_range(para, &options)?
                .expect("a once-occurring expect plans a splice");
        let BlockNode::Paragraph(para_mut) = &mut *block else {
            unreachable!("checked paragraph above");
        };
        super::super::apply_span_splice(
            para_mut,
            (start, end),
            &splice,
            revision,
            None,
            rev_counter,
            mode == MaterializationMode::Direct,
        );
        return Ok(());
    }

    // Opaque-preservation + preserved-inline-order validation (OpaqueDestroyed on
    // a PAGE field run dropped, etc.). Same check the body path runs.
    let anchors = collect_anchor_inventory(para);
    validate_preserved_inlines(para, block_id, content, &anchors, step_index)?;
    let without_range_refs = super::super::without_comment_range_refs(para, content);
    let content = &without_range_refs;

    // Identity short-circuit: a no-op replace must not author empty changes.
    if is_identity_replacement(para, content) {
        return Ok(());
    }

    // Lower via the body materializer. Mark-bearing content routes to the
    // whole-paragraph segment replace; everything else to the word-level diff —
    // identical dispatch to the body `ReplaceParagraphText` arm.
    let needs_segment_replace = content.fragments.iter().any(|f| f.is_styled());
    let BlockNode::Paragraph(para_mut) = &mut *block else {
        unreachable!("checked paragraph above");
    };
    // No enclosing block insertion: `validate_block_is_editable` above refuses a
    // pending-inserted (or otherwise tracked) story block, so this paragraph is
    // never itself part of a block insertion.
    if needs_segment_replace {
        apply_segment_replace_paragraph(para_mut, content, revision, None, rev_counter);
    } else {
        apply_replace_paragraph_text(para_mut, content, revision, None, rev_counter);
    }

    // Direct mode: resolve the freshly-authored tracked changes so the story
    // paragraph returns to all-Normal (mirrors the body Direct path).
    if mode == MaterializationMode::Direct {
        project_block_for_accept_reject(block, true);
    }

    Ok(())
}

/// The replacement text when `content` replaces only the part of `para` that
/// `expect` names: `expect` is part of the paragraph's text (not all of it),
/// the content is plain text, and it repeats neither the text before nor the
/// text after that part. None when the content rebuilds the paragraph.
fn part_replacement(
    para: &ParagraphNode,
    expect: &str,
    content: &ParagraphContent,
) -> Option<String> {
    let replacement: String = content
        .fragments
        .iter()
        .map(|fragment| match fragment {
            crate::edit::ContentFragment::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Option<String>>()?;
    let text = paragraph_visible_text(para);
    if expect.trim().is_empty() || expect.trim() == text.trim() {
        return None;
    }
    let at = text.find(expect)?;
    let before = text[..at].trim();
    let after = text[at + expect.len()..].trim();
    let repeats = |part: &str| !part.is_empty() && replacement.contains(part);
    (!repeats(before) && !repeats(after)).then_some(replacement)
}

/// `CreateHeader` / `CreateFooter` — author a NET-NEW, blank header/footer story
/// (§17.10.1/§17.10.2) and reference it from the body section.
///
/// Three coordinated mutations, all on the modeled IR (no PendingParts, no second
/// materializer — Invariant M holds):
///   1. push a blank `HeaderStory`/`FooterStory` (one empty paragraph) into
///      `doc.headers`/`doc.footers` under a freshly-allocated part name. The save
///      path synthesizes the OPC part + content-type + document rel for a story
///      present in neither base nor target (`load_story_template_root` +
///      `resolve_story_part_to_rid`), so creating the story object is enough.
///   2. add a `headerReference`/`footerReference` of the requested kind to the
///      body section, pointing at the new part.
///   3. FORCE the tracked modeled-sectPr path by recording a
///      `body_section_property_change` (a `w:sectPrChange`, §17.13.5.32) whose
///      previous-state inner `w:sectPr` has the reference STRIPPED (CT_SectPrBase
///      carries no `EG_HdrFtrReferences`). On accept the new reference stays; on
///      reject the previous sectPr is restored (no reference) and the now-blank,
///      unreferenced story is pruned (see `project_body_section_for_accept_reject`).
///
/// Fail-loud preconditions:
///   - a header/footer of the requested kind already referenced on the section →
///     `HeaderFooterAlreadyExists` (refuse to duplicate; point to EditHeader).
///   - the body section already carries a tracked `sectPrChange` →
///     `SectionAlreadyHasTrackedChange` (accept/reject it first; mirrors
///     page-setup's guard, so we never stack two sectPrChange records).
pub(crate) fn apply_create(
    doc: &mut CanonDoc,
    is_header: bool,
    kind: &HeaderFooterKind,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse a duplicate: a reference of this kind must not already exist ON THE
    // BODY SECTION. "Exists on the section" — not merely "a story object exists"
    // — is the right gate: the importer always materializes a blank Default
    // header reference (and a First reference when titlePg is set) per §17.10.2,
    // so a section's Default running head already exists and should be EDITED
    // (EditHeader fills the blank), never re-created. An `Even` header (and a
    // `First` header on a section without titlePg) is the genuinely net-new case
    // this verb authors.
    let kind_already_referenced = doc
        .body_section_properties
        .as_ref()
        .map(|sp| {
            let refs = if is_header {
                &sp.header_refs
            } else {
                &sp.footer_refs
            };
            refs.iter().any(|r| &r.kind == kind)
        })
        .unwrap_or(false);
    if kind_already_referenced {
        return Err(EditError::HeaderFooterAlreadyExists {
            is_header,
            kind: header_footer_kind_label(kind),
            step_index,
        });
    }

    // Refuse to stack a second tracked sectPrChange on the body section — accept
    // or reject the existing one first (mirrors the page-setup guard).
    if doc.body_section_property_change.is_some() {
        return Err(EditError::SectionAlreadyHasTrackedChange {
            block_id: None,
            step_index,
        });
    }

    // Allocate a part name that collides with no existing header/footer story.
    let part_name = allocate_story_part_name(doc, is_header);

    // The body section must exist as a typed value so we can add the reference
    // and snapshot the previous state. The body legitimately may have none yet;
    // default it (an empty sectPr is valid and is what an unset body implies).
    let sp = doc
        .body_section_properties
        .get_or_insert_with(SectionProperties::default);

    // Snapshot the PREVIOUS section state BEFORE adding the new reference — the
    // COMPLETE previous state INCLUDING any existing header/footer references
    // (e.g. the importer's synthesized-blank Default reference). This is the same
    // snapshot the sibling `SetPageSetup` verb records (`previous_sect_pr_raw`):
    // on reject the previous sectPr is restored verbatim, so the section returns
    // to EXACTLY its pre-create state (the new reference gone, every prior
    // reference intact). We deliberately do NOT strip header refs here — keeping
    // them is what makes reject-all == the original section.
    let previous = sp.clone();

    // Add the new reference to the live section.
    let new_ref = crate::domain::StoryRef {
        kind: kind.clone(),
        part_path: part_name.clone(),
        source_order: None,
        synthesized: false,
    };
    if is_header {
        sp.header_refs.push(new_ref);
    } else {
        sp.footer_refs.push(new_ref);
    }

    // Push the blank story (one empty paragraph). The save path synthesizes the
    // part/content-type/rel for a story present in neither base nor target.
    //
    // WORD RULE (verified against real Word): Word
    // never registers a reference-only `sectPrChange` as a revision — its own
    // writer adds header references UNTRACKED and tracks the story CONTENT
    // instead. So in tracked mode the blank paragraph carries an inserted
    // paragraph mark (§17.13.5.19): that is the Word-visible face of the
    // creation (an `insert` revision in the story), while the sectPrChange
    // remains stemma's own reject bookkeeping for the reference.
    let blank_block = match mode {
        MaterializationMode::TrackedChange => TrackedBlock {
            status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
            ..blank_story_paragraph(&part_name)
        },
        MaterializationMode::Direct => blank_story_paragraph(&part_name),
    };
    let blocks = vec![blank_block];
    // Real parts precede the blank stories synthesized for sections without
    // a reference (§17.10.5), the order import reads them in.
    if is_header {
        let position = doc
            .headers
            .iter()
            .position(|story| story.synthesized)
            .unwrap_or(doc.headers.len());
        doc.headers.insert(
            position,
            HeaderStory {
                part_name,
                kind: kind.clone(),
                blocks,
                content_hash: String::new(),
                synthesized: false,
            },
        );
    } else {
        let position = doc
            .footers
            .iter()
            .position(|story| story.synthesized)
            .unwrap_or(doc.footers.len());
        doc.footers.insert(
            position,
            FooterStory {
                part_name,
                kind: kind.clone(),
                blocks,
                content_hash: String::new(),
                synthesized: false,
            },
        );
    }

    // Force the tracked sectPr path so the new reference is a reviewable
    // `w:sectPrChange`. In Direct mode the reference is applied with no change
    // record (mirrors page-setup's Direct branch).
    match mode {
        MaterializationMode::TrackedChange => {
            let rev = next_revision(revision, rev_counter);
            doc.body_section_property_change = Some(SectionPropertyChange {
                revision: rev,
                previous_properties_raw: super::page_setup::previous_sect_pr_raw(&previous),
            });
        }
        MaterializationMode::Direct => {
            doc.body_section_property_change = None;
        }
    }
    Ok(())
}

/// Allocate a `headerN.xml`/`footerN.xml` part name that collides with no
/// existing header OR footer story part. Both families share the `word/` folder,
/// so we scan both to avoid a name clash across kinds.
fn allocate_story_part_name(doc: &CanonDoc, is_header: bool) -> String {
    let stem = if is_header { "header" } else { "footer" };
    let used: std::collections::HashSet<&str> = doc
        .headers
        .iter()
        .map(|h| h.part_name.as_str())
        .chain(doc.footers.iter().map(|f| f.part_name.as_str()))
        .collect();
    // Start at 1 and walk up to the first free `{stem}{n}.xml`.
    let mut n = 1u32;
    loop {
        let candidate = format!("{stem}{n}.xml");
        if !used.contains(candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

/// A blank story body: one empty Normal paragraph. A truly empty `w:hdr`/`w:ftr`
/// is valid OOXML, but Word expects at least one block-level child, so we author
/// a single empty paragraph the running head can later be edited into.
fn blank_story_paragraph(part_name: &str) -> TrackedBlock {
    // Derive a stable, part-scoped block id so the new paragraph is addressable
    // by a follow-up EditHeader/EditFooter without colliding with the body's
    // `p_{n}` namespace.
    let stem = part_name.trim_end_matches(".xml");
    let para = ParagraphNode::new_story_body(&format!("{stem}_p1"), "", None);
    normal_tracked_block(BlockNode::Paragraph(Box::new(para)))
}

/// `SetHeaderFooterMode` — toggle `w:titlePg` (§17.6.18) and/or
/// `w:evenAndOddHeaders` (§17.15.1.35), and link/unlink a section's header/footer
/// references by kind. The target section is the body section in v1.
pub(crate) fn apply_set_mode(
    doc: &mut CanonDoc,
    title_page: Option<bool>,
    even_and_odd: Option<bool>,
    link: Option<HeaderFooterLink>,
    step_index: usize,
    explicitly_unlinked: &mut std::collections::HashSet<String>,
) -> Result<(), EditError> {
    // Refuse a fully-empty request — no silent no-op.
    if title_page.is_none() && even_and_odd.is_none() && link.is_none() {
        return Err(EditError::NoHeaderFooterModeRequested { step_index });
    }

    // titlePg lives on the section. v1 targets the body section; it must exist.
    if let Some(tp) = title_page {
        let sp = doc
            .body_section_properties
            .get_or_insert_with(SectionProperties::default);
        sp.title_page = Some(tp);
    }

    // evenAndOddHeaders is a document-level setting; we carry the three-state
    // value honestly (Some(true)/Some(false)) so the writer round-trips it.
    if let Some(eo) = even_and_odd {
        doc.even_and_odd_headers = Some(eo);
    }

    // Link / unlink a header/footer reference on the body section.
    if let Some(link_op) = link {
        apply_link(doc, &link_op, step_index, explicitly_unlinked)?;
    }

    Ok(())
}

/// Link or unlink a header/footer reference of the given kind on the body
/// section. LINK resolves an EXISTING story (by kind → part name); a kind with
/// no existing story fails `HeaderFooterRefNotResolvable` (net-new-story
/// creation is out of v1). UNLINK drops the matching reference.
fn apply_link(
    doc: &mut CanonDoc,
    link_op: &HeaderFooterLink,
    step_index: usize,
    explicitly_unlinked: &mut std::collections::HashSet<String>,
) -> Result<(), EditError> {
    // Resolve an existing story part for the requested kind BEFORE borrowing the
    // section mutably (LINK needs it; UNLINK does not).
    let part_for_link = if link_op.link {
        let resolved = if link_op.is_header {
            header_part_for_kind(doc, &link_op.kind)
        } else {
            footer_part_for_kind(doc, &link_op.kind)
        };
        match resolved {
            Some(p) => Some(p),
            None => {
                return Err(EditError::HeaderFooterRefNotResolvable {
                    is_header: link_op.is_header,
                    kind: header_footer_kind_label(&link_op.kind),
                    step_index,
                });
            }
        }
    } else {
        None
    };

    let sp = doc
        .body_section_properties
        .get_or_insert_with(SectionProperties::default);
    let refs = if link_op.is_header {
        &mut sp.header_refs
    } else {
        &mut sp.footer_refs
    };

    if link_op.link {
        let part_path = part_for_link.expect("link path resolved above");
        // Idempotent: don't duplicate an existing reference of this kind.
        if refs.iter().any(|r| r.kind == link_op.kind) {
            return Ok(());
        }
        refs.push(crate::domain::StoryRef {
            kind: link_op.kind.clone(),
            part_path,
            source_order: None,
            synthesized: false,
        });
    } else {
        // Unlink: drop every reference of the requested kind. Record the parts
        // it detached — the story itself survives an unlink and stays
        // addressable by kind, so end-of-transaction orphan pruning must not
        // treat these as garbage (see `prune_newly_unreferenced_stories`).
        for detached in refs.iter().filter(|r| r.kind == link_op.kind) {
            explicitly_unlinked.insert(detached.part_path.clone());
        }
        refs.retain(|r| r.kind != link_op.kind);
    }
    Ok(())
}

fn header_footer_kind_label(kind: &HeaderFooterKind) -> &'static str {
    match kind {
        HeaderFooterKind::Default => "default",
        HeaderFooterKind::First => "first",
        HeaderFooterKind::Even => "even",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn empty_mode_request_is_refused() {
        let mut doc = empty_doc();
        let err = apply_set_mode(
            &mut doc,
            None,
            None,
            None,
            0,
            &mut std::collections::HashSet::new(),
        )
        .expect_err("empty mode request must be refused");
        assert!(matches!(err, EditError::NoHeaderFooterModeRequested { .. }));
    }

    #[test]
    fn title_page_toggle_sets_section_flag() {
        let mut doc = empty_doc();
        apply_set_mode(
            &mut doc,
            Some(true),
            None,
            None,
            0,
            &mut std::collections::HashSet::new(),
        )
        .expect("toggle ok");
        assert_eq!(doc.body_section_properties.unwrap().title_page, Some(true));
    }

    #[test]
    fn even_and_odd_toggle_records_three_state() {
        let mut doc = empty_doc();
        apply_set_mode(
            &mut doc,
            None,
            Some(true),
            None,
            0,
            &mut std::collections::HashSet::new(),
        )
        .expect("toggle ok");
        assert_eq!(doc.even_and_odd_headers, Some(true));
        apply_set_mode(
            &mut doc,
            None,
            Some(false),
            None,
            0,
            &mut std::collections::HashSet::new(),
        )
        .expect("toggle ok");
        assert_eq!(doc.even_and_odd_headers, Some(false));
    }

    #[test]
    fn link_to_missing_header_story_fails_loud() {
        let mut doc = empty_doc();
        let err = apply_set_mode(
            &mut doc,
            None,
            None,
            Some(HeaderFooterLink {
                is_header: true,
                kind: HeaderFooterKind::Default,
                link: true,
            }),
            0,
            &mut std::collections::HashSet::new(),
        )
        .expect_err("link to non-existent header must fail");
        assert!(matches!(
            err,
            EditError::HeaderFooterRefNotResolvable { .. }
        ));
    }

    #[test]
    fn link_existing_header_adds_reference_then_unlink_removes() {
        let mut doc = empty_doc();
        doc.headers.push(crate::domain::HeaderStory {
            part_name: "header1.xml".to_string(),
            kind: HeaderFooterKind::Default,
            blocks: vec![],
            content_hash: String::new(),
            synthesized: false,
        });
        apply_set_mode(
            &mut doc,
            None,
            None,
            Some(HeaderFooterLink {
                is_header: true,
                kind: HeaderFooterKind::Default,
                link: true,
            }),
            0,
            &mut std::collections::HashSet::new(),
        )
        .expect("link ok");
        let refs = &doc.body_section_properties.as_ref().unwrap().header_refs;
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].part_path, "header1.xml");

        apply_set_mode(
            &mut doc,
            None,
            None,
            Some(HeaderFooterLink {
                is_header: true,
                kind: HeaderFooterKind::Default,
                link: false,
            }),
            0,
            &mut std::collections::HashSet::new(),
        )
        .expect("unlink ok");
        assert!(
            doc.body_section_properties
                .as_ref()
                .unwrap()
                .header_refs
                .is_empty()
        );
    }
}
