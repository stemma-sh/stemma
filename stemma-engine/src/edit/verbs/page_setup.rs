//! Section / page-setup authoring verbs (§17.6). "Make this section landscape;
//! set 1-inch margins; two columns; insert a section break here."
//!
//! These are **section-property deltas**, NOT text edits: they mutate the
//! `SectionProperties` attached to the body (`doc.body_section_properties`) or
//! to a paragraph (`para.section_properties`, a mid-document section break) in
//! place. They do **not** route through the segment materializer (Invariant M);
//! a `w:sectPr` child change is a property delta, exactly like the `w:pPrChange`
//! lift in `paragraph_formatting.rs`.
//!
//! In `TrackedChange` mode a `SetPageSetup` records the prior `w:sectPr` as a
//! `SectionPropertyChange` (`w:sectPrChange`, §17.13.5.32) — the raw previous
//! state — so accept-all keeps the new layout and reject-all restores the
//! original. The serializer already emits `w:sectPrChange` from that field and
//! `normalize.rs` already does byte-level accept/reject of the wrapper.
//!
//! §17.6.22 Continuous-section inheritance: a `Continuous` section inherits its
//! page properties from the **preceding** section (a known drafting-error
//! workaround the import already implements via
//! `propagate_continuous_section_properties`). `SetSectionType` only flips the
//! `section_type`; it never invents page geometry, so the preceding-section
//! inheritance the importer established is preserved untouched.
//!
//! v1 scope (fail loud beyond it):
//! - `SetPageSetup` / `SetSectionType` target the body section or a top-level
//!   paragraph that already owns a `w:sectPr`; addressing a paragraph with no
//!   section break surfaces `SectionPropertiesNotFound`.
//! - `InsertSectionBreak` attaches a section break to a top-level paragraph that
//!   does **not** already own one (a paragraph already carrying a `sectPr`
//!   surfaces `SectionPropertiesNotFound`'s sibling guard — we refuse to clobber
//!   it).
//! - an empty patch is refused (`NoPageSetupRequested`); a no-op (patch equals
//!   current) is silently skipped (no empty `sectPrChange`).
//! - a section that already carries a tracked `sectPrChange` is refused
//!   (`SectionAlreadyHasTrackedChange`) — accept/reject it first.

use super::super::{EditError, MaterializationMode, find_block_index, next_revision};
use crate::domain::{
    BlockNode, CanonDoc, NodeId, PageOrientation, RevisionInfo, SectionProperties,
    SectionPropertyChange, SectionType,
};

/// Which section a page-setup verb targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionTarget {
    /// The document-level (final) section: `doc.body_section_properties`.
    Body,
    /// A mid-document section break attached to a top-level paragraph's
    /// `w:sectPr` (the paragraph that ends the section).
    Paragraph(NodeId),
}

/// A page size in twips (`w:pgSz`, §17.6.13). Width/height are the raw stored
/// values; orientation rides separately so a caller can flip orientation
/// without restating dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSize {
    pub width: u32,
    pub height: u32,
}

/// Page margins in twips (`w:pgMar`, §17.6.11). All four edges plus
/// header/footer distance are required together when a margins patch is given
/// (a partial margin box is ambiguous and refused at the wire edge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageMargins {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
    pub header: u32,
    pub footer: u32,
}

/// A column layout (`w:cols`, §17.6.4): equal-width columns with a gutter
/// (`w:space`) between them. v1 authors only equal-width columns; per-column
/// widths (`column_defs`) stay role-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnLayout {
    pub count: u32,
    pub space: u32,
}

/// All-`Option` page-setup patch. Each `Some` field overwrites the matching
/// section property; `None` leaves it untouched. An all-`None` patch is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageSetupPatch {
    pub page_size: Option<PageSize>,
    pub orientation: Option<PageOrientation>,
    pub margins: Option<PageMargins>,
    pub columns: Option<ColumnLayout>,
    /// Gutter margin (`w:pgMar w:gutter`, §17.6.15) in twips.
    pub gutter: Option<u32>,
}

impl PageSetupPatch {
    pub(crate) fn is_empty(&self) -> bool {
        self.page_size.is_none()
            && self.orientation.is_none()
            && self.margins.is_none()
            && self.columns.is_none()
            && self.gutter.is_none()
    }

    /// Apply this patch to a `SectionProperties` in place. Returns `true` if any
    /// field actually changed (used to short-circuit no-op edits).
    fn apply_to(&self, sp: &mut SectionProperties) -> bool {
        let before = sp.clone();
        if let Some(size) = self.page_size {
            sp.page_width = Some(size.width);
            sp.page_height = Some(size.height);
        }
        if let Some(orient) = &self.orientation {
            sp.orientation = Some(orient.clone());
            // Reconcile page dimensions with the requested orientation unless
            // the caller stated them explicitly. `w:orient` is descriptive —
            // the page renders from w/h — so "make it landscape" must swap a
            // portrait-shaped w/h (Word's own writer does), and on a section
            // with no authored pgSz it must materialize dimensions or the
            // flag changes nothing on screen. Shape-driven and idempotent:
            // dims already agreeing with the orientation are left alone.
            if self.page_size.is_none() {
                match (sp.page_width, sp.page_height) {
                    (Some(w), Some(h)) => {
                        let wants_landscape = matches!(orient, PageOrientation::Landscape);
                        if wants_landscape != (w > h) && w != h {
                            sp.page_width = Some(h);
                            sp.page_height = Some(w);
                        }
                    }
                    (None, None) => {
                        let (w, h) = match orient {
                            PageOrientation::Landscape => {
                                (WORD_DEFAULT_PAGE_HEIGHT, WORD_DEFAULT_PAGE_WIDTH)
                            }
                            PageOrientation::Portrait => {
                                (WORD_DEFAULT_PAGE_WIDTH, WORD_DEFAULT_PAGE_HEIGHT)
                            }
                        };
                        sp.page_width = Some(w);
                        sp.page_height = Some(h);
                    }
                    // Half-authored pgSz (only one of w/h): refuse to guess the
                    // missing dimension — leave the authored value untouched.
                    _ => {}
                }
            }
        }
        if let Some(m) = self.margins {
            sp.margin_top = Some(m.top);
            sp.margin_bottom = Some(m.bottom);
            sp.margin_left = Some(m.left);
            sp.margin_right = Some(m.right);
            sp.header_distance = Some(m.header);
            sp.footer_distance = Some(m.footer);
        }
        if let Some(cols) = self.columns {
            sp.columns = Some(cols.count);
            sp.column_space = Some(cols.space);
            // Equal-width: drop any per-column overrides so the layout is honest.
            sp.column_defs.clear();
        }
        if let Some(gutter) = self.gutter {
            sp.gutter = Some(gutter);
        }
        *sp != before
    }
}

/// Word's default page geometry (US Letter portrait, 1" margins, ½"
/// header/footer distance) — what Word itself materializes when a section
/// has no authored geometry (observed verbatim in real Word's own
/// output). Shared with the serializer's empty-snapshot
/// materialization (`runtime::materialize_empty_sect_pr_snapshot`).
pub(crate) const WORD_DEFAULT_PAGE_WIDTH: u32 = 12240;
pub(crate) const WORD_DEFAULT_PAGE_HEIGHT: u32 = 15840;
pub(crate) const WORD_DEFAULT_MARGIN: i32 = 1440;
pub(crate) const WORD_DEFAULT_HEADER_FOOTER_DISTANCE: u32 = 720;

/// Build the raw `w:sectPr` bytes for a previous-state snapshot (the inner
/// element of `w:sectPrChange`, §17.13.5.32). Reuses the serializer's
/// `section_properties_to_element` (the ONE sectPr emitter) so the recorded
/// previous state matches what import would have parsed — then re-serializes
/// with the raw-fragment writer the import path's `serialize_element` pairs with.
///
/// The snapshot stays FAITHFUL to the authored previous state — possibly
/// empty. stemma's own reject restores it verbatim; the Word-interop
/// materialization for an empty snapshot (Word drops the revision otherwise)
/// happens at the write edge, in `runtime::materialize_empty_sect_pr_snapshot`.
pub(crate) fn previous_sect_pr_raw(prev: &SectionProperties) -> Vec<u8> {
    let el = crate::runtime::section_properties_to_element(prev, None, None, None);
    crate::word_xml::serialize_raw_fragment(&el)
}

/// Resolve the target section's `(properties, change)` slots mutably.
///
/// Body → `doc.body_section_properties` / `doc.body_section_property_change`.
/// Paragraph → that paragraph's `section_properties` / `section_property_change`
/// (a mid-document break). A paragraph with no `w:sectPr` surfaces
/// `SectionPropertiesNotFound` — we never fabricate a section break here
/// (that is `InsertSectionBreak`'s job).
fn resolve_section_mut<'a>(
    doc: &'a mut CanonDoc,
    target: &SectionTarget,
    step_index: usize,
) -> Result<
    (
        &'a mut Option<SectionProperties>,
        &'a mut Option<SectionPropertyChange>,
    ),
    EditError,
> {
    match target {
        SectionTarget::Body => Ok((
            &mut doc.body_section_properties,
            &mut doc.body_section_property_change,
        )),
        SectionTarget::Paragraph(block_id) => {
            let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| {
                EditError::BlockNotFound {
                    block_id: block_id.clone(),
                    step_index,
                }
            })?;
            match &mut doc.blocks[idx].block {
                BlockNode::Paragraph(p) => {
                    if p.section_properties.is_none() {
                        return Err(EditError::SectionPropertiesNotFound {
                            block_id: Some(block_id.clone()),
                            step_index,
                        });
                    }
                    Ok((&mut p.section_properties, &mut p.section_property_change))
                }
                _ => Err(EditError::SectionPropertiesNotFound {
                    block_id: Some(block_id.clone()),
                    step_index,
                }),
            }
        }
    }
}

/// `EditStep::SetPageSetup` — patch the target section's `w:sectPr` children,
/// recording the prior state as a `w:sectPrChange` in `TrackedChange` mode.
pub(crate) fn apply_set_page_setup(
    doc: &mut CanonDoc,
    target: &SectionTarget,
    patch: &PageSetupPatch,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse a no-op request before touching anything (no empty sectPrChange).
    if patch.is_empty() {
        return Err(EditError::NoPageSetupRequested { step_index });
    }

    let (props_slot, change_slot) = resolve_section_mut(doc, target, step_index)?;

    // The section must exist. The body may legitimately have none yet; a
    // page-setup edit on a section that does not exist is a hard error rather
    // than silently materializing a default sectPr.
    let Some(sp) = props_slot.as_mut() else {
        return Err(EditError::SectionPropertiesNotFound {
            block_id: target_block_id(target),
            step_index,
        });
    };

    // Refuse to stack a new tracked sectPrChange on a section that already
    // carries one — accept or reject it first (mirrors the pPrChange guard).
    if change_slot.is_some() {
        return Err(EditError::SectionAlreadyHasTrackedChange {
            block_id: target_block_id(target),
            step_index,
        });
    }

    // Snapshot BEFORE mutating so the sectPrChange inner sectPr is the complete
    // previous state.
    let previous = sp.clone();
    let changed = patch.apply_to(sp);
    if !changed {
        // No visible change → no empty sectPrChange (no-op short-circuit).
        return Ok(());
    }

    match mode {
        MaterializationMode::TrackedChange => {
            let rev = next_revision(revision, rev_counter);
            *change_slot = Some(SectionPropertyChange {
                revision: rev,
                previous_properties_raw: previous_sect_pr_raw(&previous),
            });
        }
        MaterializationMode::Direct => {
            *change_slot = None;
        }
    }
    Ok(())
}

/// `EditStep::SetSectionType` — set the section's `w:type` (§17.6.22). Only the
/// `section_type` discriminant changes; page geometry (and therefore the
/// preceding-section inheritance the importer established for a `Continuous`
/// section) is left untouched.
pub(crate) fn apply_set_section_type(
    doc: &mut CanonDoc,
    target: &SectionTarget,
    section_type: SectionType,
    step_index: usize,
) -> Result<(), EditError> {
    let (props_slot, _change_slot) = resolve_section_mut(doc, target, step_index)?;
    let Some(sp) = props_slot.as_mut() else {
        return Err(EditError::SectionPropertiesNotFound {
            block_id: target_block_id(target),
            step_index,
        });
    };
    // No-op short-circuit: setting the current type changes nothing.
    if sp.section_type.as_ref() == Some(&section_type) {
        return Ok(());
    }
    sp.section_type = Some(section_type);
    Ok(())
}

/// `EditStep::InsertSectionBreak` — attach a fresh mid-document section break to
/// the anchor paragraph's `w:sectPr`. The paragraph must NOT already own one
/// (we refuse to clobber an existing break). The serializer emits the
/// `w:sectPr` in that paragraph's `w:pPr`.
pub(crate) fn apply_insert_section_break(
    doc: &mut CanonDoc,
    anchor_block_id: &NodeId,
    section_type: SectionType,
    properties: &PageSetupPatch,
    step_index: usize,
) -> Result<(), EditError> {
    let idx =
        find_block_index(&doc.blocks, anchor_block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: anchor_block_id.clone(),
            step_index,
        })?;
    let BlockNode::Paragraph(p) = &mut doc.blocks[idx].block else {
        return Err(EditError::NotAParagraph {
            block_id: anchor_block_id.clone(),
            actual_kind: block_kind_label(&doc.blocks[idx].block),
            step_index,
        });
    };
    // Refuse to clobber an existing section break — that would silently drop the
    // current layout. The caller should SetPageSetup the existing break instead.
    if p.section_properties.is_some() {
        return Err(EditError::SectionAlreadyHasTrackedChange {
            block_id: Some(anchor_block_id.clone()),
            step_index,
        });
    }

    let mut sp = SectionProperties {
        section_type: Some(section_type),
        ..Default::default()
    };
    // Fold the requested geometry into the new break. (An empty patch is a
    // legitimate "just a section break with default/inherited geometry"; we do
    // not refuse it here — the break itself is the requested change.)
    let _ = properties.apply_to(&mut sp);
    p.section_properties = Some(sp);
    Ok(())
}

fn target_block_id(target: &SectionTarget) -> Option<NodeId> {
    match target {
        SectionTarget::Body => None,
        SectionTarget::Paragraph(id) => Some(id.clone()),
    }
}

fn block_kind_label(block: &BlockNode) -> &'static str {
    match block {
        BlockNode::Paragraph(_) => "paragraph",
        BlockNode::Table(_) => "table",
        BlockNode::OpaqueBlock(_) => "opaque_block",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_patch_is_empty() {
        assert!(PageSetupPatch::default().is_empty());
    }

    #[test]
    fn orientation_patch_is_not_empty_and_changes() {
        let patch = PageSetupPatch {
            orientation: Some(PageOrientation::Landscape),
            ..Default::default()
        };
        assert!(!patch.is_empty());
        let mut sp = SectionProperties::default();
        assert!(patch.apply_to(&mut sp));
        assert_eq!(sp.orientation, Some(PageOrientation::Landscape));
        // Re-applying the same value is a no-op.
        assert!(!patch.apply_to(&mut sp));
    }

    #[test]
    fn columns_patch_clears_per_column_overrides() {
        let mut sp = SectionProperties {
            column_defs: vec![crate::domain::ColumnDef {
                width: 100,
                space: 10,
            }],
            ..Default::default()
        };
        let patch = PageSetupPatch {
            columns: Some(ColumnLayout {
                count: 2,
                space: 720,
            }),
            ..Default::default()
        };
        assert!(patch.apply_to(&mut sp));
        assert_eq!(sp.columns, Some(2));
        assert_eq!(sp.column_space, Some(720));
        assert!(sp.column_defs.is_empty());
    }
}
