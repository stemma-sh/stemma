//! DOCX import / parsing pipeline.
//!
//! Extracts domain types from OOXML XML elements. This module is the
//! "XML → domain" half of the runtime; the "domain → XML" serialization
//! lives in `runtime.rs`.

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};
use xmltree::{Element, XMLNode};

use crate::docx::{DocxArchive, DocxError};
use crate::domain::{
    Alignment, BlockNode, BlockSdtWrap, Border, BorderSet, BorderStyle, CanonDoc, CellFormatting,
    CellFormattingChange, CellMargins, CellSdtWrap, CommentExtended, CommentPayload, CommentStory,
    CompatSettings, DecorationNode, DecorationType, DocFingerprint, DocMeta, DocPart,
    DocProtectEdit, EmphasisMark, EndnoteStory, FieldData, FieldKind, FitText, FooterStory,
    FootnoteStory, FormattingChange, FullDocBlock, FullDocViewResult, HAnchor, HeaderFooterKind,
    HeaderStory, HeadingLevel, HeightRule, HighlightColor, INTERNAL_IDS_VERSION_V0, IStr,
    Indentation, InlineChange, InlineChangeSegmentType, InlineNode, LineSpacingRule, Mark,
    MarkValue, NodeId, NoteReferenceData, NoteType, OpaqueBlockNode, OpaqueKind, ParagraphBorders,
    ParagraphFormattingChange, ParagraphNode, ParagraphSpacing, ProofRef, RangeMarkerMeta,
    RevisionInfo, RowFormattingChange, RunRprAuthored, SCHEMA_VERSION_V0, SdtWrapper,
    SectionPropertyChange, SectionType, Shading, ShadingPattern, StoryPayload, StyleProps, SymData,
    TableCellNode, TableFormatting, TableFormattingChange, TableLayout, TableMeasurement,
    TableNode, TableOverlap, TablePositioning, TableRowNode, TblLook, TextDirection, TextEffect,
    TextNode, TrackedBlock, TrackedSegment, TrackingStatus, UnderlineStyle, VAnchor,
    VerticalAlignment, VerticalMerge, WidthType, XAlign, YAlign, normal_tracked_block,
};
use crate::runtime::{
    COMMENTS_EXTENDED_REL_TYPE, COMMENTS_REL_TYPE, CUSTOM_XML_REL_TYPE, Diagnostic,
    DiagnosticLevel, DocumentRelationships, ENDNOTES_REL_TYPE, ErrorCode, ErrorDetails,
    FOOTER_REL_TYPE, FOOTNOTES_REL_TYPE, HEADER_REL_TYPE, HYPERLINK_REL_TYPE, HeaderFooterRef,
    Relationship, RuntimeError,
};
use crate::word_ir::{
    Atom, AtomKind, AtomTrackingContext, MarkValue as WordMarkValue, ParagraphView, TextMarks,
    WordIrError, is_mc_alternate_content, select_mc_branch,
};
use crate::word_xml::{self, WordXmlError, body_element, is_w_tag};
use crate::xml_attrs::attr_get;

// =============================================================================
// Shared helpers (used by both import and runtime, re-exported as pub(crate))
// =============================================================================

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn invalid_docx(message: &str) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: message.to_string(),
        details: ErrorDetails::default(),
    }
}

pub(crate) fn invalid_docx_message(message: String) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message,
        details: ErrorDetails::default(),
    }
}

pub(crate) fn map_package_error(err: crate::docx_package::PackageError) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: err.to_string(),
        details: ErrorDetails::default(),
    }
}

/// Resolve a relationship `Target` (from the main part's `.rels`) to a stored
/// part name. A leading-`/` target is package-root-absolute; a relative target
/// resolves against `main_dir` (the main document part's directory), per OPC
/// pack-URI resolution. This replaces a hardcoded `word/` base so a main part
/// stored outside the conventional directory still locates its sibling parts.
pub(crate) fn resolve_relationship_target(target: &str, main_dir: &str) -> String {
    if target.starts_with('/') {
        crate::docx_package::normalize_package_path(target)
    } else {
        crate::docx_package::normalize_package_path(&format!("{main_dir}{target}"))
    }
}

fn map_docx_error(err: DocxError) -> RuntimeError {
    let message = match err {
        DocxError::ZipRead(source) => format!("docx read failed: {source}"),
        DocxError::ZipWrite(source) => format!("docx write failed: {source}"),
        DocxError::Io(source) => format!("docx io error: {source}"),
        DocxError::MissingFile(name) => format!("docx missing file: {name}"),
        DocxError::ZipBomb(detail) => format!("docx rejected: {detail}"),
        DocxError::DuplicatePartName { name, existing } => format!(
            "docx rejected: duplicate ZIP part name {name:?} (case-equivalent to {existing:?}); \
             part names must be unique (OPC §6.2, §7.3) — Word reports such packages as corrupt"
        ),
    };
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message,
        details: ErrorDetails::default(),
    }
}

pub(crate) fn parse_optional_docx_part<T, F>(
    archive: &DocxArchive,
    part_name: &str,
    parse: F,
) -> Result<Option<T>, RuntimeError>
where
    F: FnOnce(&[u8]) -> Result<T, String>,
{
    let Some(xml_bytes) = archive.get(part_name) else {
        return Ok(None);
    };

    parse(xml_bytes).map(Some).map_err(invalid_docx_message)
}

fn map_word_xml_error(err: WordXmlError) -> RuntimeError {
    let message = match err {
        WordXmlError::XmlParse(source) => format!("wordprocessingml parse error: {source}"),
        WordXmlError::XmlDepthExceeded { limit, depth } => {
            format!("wordprocessingml nesting depth {depth} exceeds supported limit {limit}")
        }
        WordXmlError::XmlWrite(source) => format!("wordprocessingml write error: {source}"),
        WordXmlError::MissingBody => "wordprocessingml missing body element".to_string(),
        WordXmlError::MultipleBody(n) => {
            format!("wordprocessingml has {n} body elements, expected exactly 1")
        }
        WordXmlError::MissingDocument => "wordprocessingml missing document element".to_string(),
        WordXmlError::QuickXml { position, reason } => {
            format!("wordprocessingml parse error at byte {position}: {reason}")
        }
        WordXmlError::DoctypeRejected => {
            "wordprocessingml contains a DOCTYPE/DTD, which is not allowed".to_string()
        }
        WordXmlError::NoRootElement => {
            "wordprocessingml stream ended without a root element".to_string()
        }
    };
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message,
        details: ErrorDetails::default(),
    }
}

fn map_word_ir_error(err: WordIrError) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("word IR error: {err}"),
        details: ErrorDetails::default(),
    }
}

fn ensure_docx_not_encrypted(archive: &DocxArchive) -> Result<(), RuntimeError> {
    const DOCX_ENCRYPTION_MARKERS: [&str; 2] = ["EncryptedPackage", "EncryptionInfo"];
    if DOCX_ENCRYPTION_MARKERS
        .iter()
        .any(|name| archive.get(name).is_some())
    {
        return Err(invalid_docx(
            "password-protected DOCX files are not supported",
        ));
    }
    Ok(())
}

/// Extract the local name from an element, stripping any namespace prefix.
pub(crate) fn local_element_name(element: &Element) -> &str {
    if let Some(pos) = element.name.find(':') {
        &element.name[pos + 1..]
    } else {
        &element.name
    }
}

/// The qualified (`prefix:local`) name of an element for `PreservedProp`.
fn qualified_prop_name(element: &Element) -> String {
    if element.name.contains(':') {
        element.name.clone()
    } else if let Some(prefix) = &element.prefix {
        format!("{prefix}:{}", element.name)
    } else {
        element.name.clone()
    }
}

/// The `w:val` attribute of the first `w:<local>` child of `props`, if present.
fn find_w_child_val(props: &Element, local: &str) -> Option<String> {
    props.children.iter().find_map(|c| match c {
        XMLNode::Element(el) if is_w_tag(el, local) => attr_get(el, "w:val").cloned(),
        _ => None,
    })
}

/// trPr children the typed `TableRowNode` fields consume. A child NOT in this
/// set (w:divId, w:hidden, foreign extensions) is captured as a preserved
/// remainder. This is the CONSUMED set, not the full CT_TrPr schema order —
/// divId/hidden are valid trPr children we don't model, so they must fall into
/// the remainder rather than be treated as "known and handled".
const TRPR_CONSUMED: &[&str] = &[
    "gridBefore",
    "gridAfter",
    "tblHeader",
    "cantSplit",
    "cnfStyle",
    "trHeight",
    "ins",
    "del",
    "trPrChange",
    "wBefore",
    "wAfter",
    "jc",
    "tblCellSpacing",
];

/// tcPr children the typed `TableCellNode` fields consume. A child NOT in this
/// set (legacy w:hMerge, foreign extensions like tm:tmTcPr) is captured as a
/// preserved remainder.
const TCPR_CONSUMED: &[&str] = &[
    "gridSpan",
    "vMerge",
    "cnfStyle",
    "hideMark",
    "vAlign",
    "noWrap",
    "textDirection",
    "tcFitText",
    "cellIns",
    "cellDel",
    "tcBorders",
    "shd",
    "tcW",
    "tcMar",
    "tcPrChange",
];

/// Capture every child element of a `tblPr`/`trPr`/`tcPr` whose local name is
/// NOT in `modeled` as a verbatim [`PreservedProp`] — the RFC-0003 "never
/// silently drop" catch-all for vendor extensions and future OOXML additions.
fn capture_unmodeled_children(
    props: &Element,
    modeled: &[&str],
) -> Vec<crate::domain::PreservedProp> {
    let mut out = Vec::new();
    for c in &props.children {
        let XMLNode::Element(el) = c else { continue };
        let local = local_element_name(el);
        if modeled.contains(&local) {
            continue;
        }
        tracing::debug!(
            element = %local,
            "capture_unmodeled_children: unmodeled table-property child captured verbatim as a preserved remainder"
        );
        out.push(crate::domain::PreservedProp {
            name: qualified_prop_name(el),
            raw_xml: String::from_utf8(crate::word_xml::serialize_raw_fragment(el))
                .expect("serialize_raw_fragment always emits valid UTF-8 XML"),
        });
    }
    out
}

/// Build a canonical from a DOCX, normalizing (accepting) any pre-existing
/// tracked changes at the XML layer first. Used by tests that exercise the
/// pre-existing-tracked-changes parse path; runtime no longer re-parses
/// because `view()` accepts tracked segments in-IR via `accept_all`.
#[allow(dead_code)]
pub(crate) fn build_canonical_from_docx(
    docx_bytes: &[u8],
    fingerprint: DocFingerprint,
) -> Result<(CanonDoc, Vec<Diagnostic>), RuntimeError> {
    let archive = DocxArchive::read(docx_bytes).map_err(map_docx_error)?;
    ensure_docx_not_encrypted(&archive)?;

    // Accept all pre-existing tracked changes before building the canonical model.
    // Without this, deleted text (w:delText) and inserted text (w:ins > w:t) both
    // end up in the canonical text, producing garbled concatenations like
    // "December 1, 2024January" instead of just "January".
    let archive = crate::normalize::normalize_if_needed(&archive).map_err(|e| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("failed to normalize pre-existing tracked changes: {e:?}"),
        details: ErrorDetails::default(),
    })?;

    build_canonical_from_archive(&archive, fingerprint)
}

/// Build a canonical document from a DOCX archive, preserving tracked changes.
///
/// Unlike `build_canonical_from_docx`, this does NOT normalize (accept) pre-existing
/// tracked changes. The resulting CanonDoc will have `TrackedSegment`s with
/// `Inserted`/`Deleted` status for inline-level tracked changes, and `TrackedBlock`s
/// with corresponding status for block-level tracked changes.
///
/// Used for single-document tracked change analysis, where we need to extract the
/// tracked changes directly rather than re-diffing.
pub fn build_canonical_from_docx_preserving_tracked(
    docx_bytes: &[u8],
    fingerprint: DocFingerprint,
) -> Result<(CanonDoc, Vec<Diagnostic>), RuntimeError> {
    let archive = DocxArchive::read(docx_bytes).map_err(map_docx_error)?;
    ensure_docx_not_encrypted(&archive)?;

    // No normalization — tracked changes are preserved as TrackedSegments.
    // Block IDs are assigned by the parse-time counter in ParseContext.
    build_canonical_from_archive(&archive, fingerprint)
}

/// Revision ids are the selector namespace: every enumerable revision must be
/// uniquely addressable. Tracked ins/del ids come from Word with document
/// scope, but FORMATTING-change ids in the wild can repeat across story parts
/// (e.g. the same `w:id` on an rPrChange in header1.xml and header2.xml —
/// Word treats them as part-local annotations). Parse at the edge: keep the
/// first occurrence of each id, re-mint any duplicate from above the
/// document-wide maximum. Well-formed inputs round-trip byte-stable; only
/// genuine duplicates are renumbered.
///
/// Runs AFTER [`mint_wire_zero_revision_ids`], so no formatting change reaches
/// here with `revision_id == 0`; the `*id == 0` skip is a defensive guard, not
/// a live path.
fn ensure_unique_formatting_change_ids(doc: &mut CanonDoc) {
    let mut next = crate::runtime::max_revision_id(doc) + 1;
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();

    let claim = |id: &mut u32, seen: &mut std::collections::HashSet<u32>, next: &mut u32| {
        if *id == 0 {
            return;
        }
        if !seen.insert(*id) {
            *id = *next;
            seen.insert(*id);
            *next += 1;
        }
    };

    fn visit_block(block: &mut BlockNode, claim: &mut impl FnMut(&mut u32)) {
        match block {
            BlockNode::Paragraph(p) => {
                if let Some(fc) = &mut p.formatting_change {
                    claim(&mut fc.revision_id);
                }
                for seg in &mut p.segments {
                    for inline in &mut seg.inlines {
                        if let crate::domain::InlineNode::Text(t) = inline
                            && let Some(fc) = &mut t.formatting_change
                        {
                            claim(&mut fc.revision_id);
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                if let Some(fc) = &mut t.formatting_change {
                    claim(&mut fc.revision_id);
                }
                for row in &mut t.rows {
                    if let Some(fc) = &mut row.formatting_change {
                        claim(&mut fc.revision_id);
                    }
                    for cell in &mut row.cells {
                        if let Some(fc) = &mut cell.formatting_change {
                            claim(&mut fc.revision_id);
                        }
                        for nested in &mut cell.blocks {
                            visit_block(nested, claim);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }

    let mut claim_fn = |id: &mut u32| claim(id, &mut seen, &mut next);
    for tb in &mut doc.blocks {
        visit_block(&mut tb.block, &mut claim_fn);
    }
    for story in &mut doc.headers {
        for tb in &mut story.blocks {
            visit_block(&mut tb.block, &mut claim_fn);
        }
    }
    for story in &mut doc.footers {
        for tb in &mut story.blocks {
            visit_block(&mut tb.block, &mut claim_fn);
        }
    }
    for story in &mut doc.footnotes {
        for tb in &mut story.blocks {
            visit_block(&mut tb.block, &mut claim_fn);
        }
    }
    for story in &mut doc.endnotes {
        for tb in &mut story.blocks {
            visit_block(&mut tb.block, &mut claim_fn);
        }
    }
    for story in &mut doc.comments {
        for tb in &mut story.blocks {
            visit_block(&mut tb.block, &mut claim_fn);
        }
    }
}

/// Wire id 0 is a legal Word revision id — real wild documents carry
/// `<w:ins w:id="0">`, `<w:del w:id="0">`, `<w:rPrChange w:id="0">`,
/// `<w:pPrChange w:id="0">`. But the engine reserves internal
/// `revision_id == 0` as the LEGACY SENTINEL for pre-identity snapshot blobs
/// (see [`crate::domain::FormattingChange::revision_id`]): a value
/// `enumerate_revisions` reports but the resolver's `revision_id != 0` guards
/// refuse. Left conflated, a wild `<w:rPrChange w:id="0">` enumerates as id 0
/// yet `resolve_selected_revisions` refuses it — breaking the enumerate↔resolve
/// agreement invariant (`enumerate_revisions_ids_agree_with_resolvable_
/// revision_ids`) — and two distinct `w:id="0"` changes collapse to one
/// unaddressable identity.
///
/// Parse at the edge: mint a fresh, document-unique id for EVERY tracked-change
/// carrier that imported with `revision_id == 0`. This is identity-honest and
/// lossless — the serializer remints every wire id on output
/// (`runtime::next_annotation_id`), so the wire id is a disposable pairing key,
/// never a value we must preserve. The legacy sentinel survives ONLY where it
/// belongs: an in-memory `CanonDoc` deserialized from a pre-identity snapshot
/// (`#[serde(default)]` on `revision_id`), which never re-enters this import
/// path — exactly the "cannot be selected by id until the doc is re-imported"
/// case the sentinel's own doc comment describes.
///
/// Walks EXACTLY the carrier set `tracked_model::resolvable_revision_ids`
/// accepts — body, every story, hyperlink-run statuses, the whole-comment
/// status, and the body-level `w:sectPrChange`. It must stay in sync with that
/// walk (and `enumerate_revisions` / `runtime::max_revision_id`): a carrier
/// added to one but not here would let a wire-0 revision on it re-open the
/// divergence.
fn mint_wire_zero_revision_ids(doc: &mut CanonDoc) {
    // Seed above every id already present (across the SAME complete carrier set
    // — `max_revision_id` covers hyperlink runs and the comment story status
    // too), so every minted id is unique against existing ids and each other.
    let mut next = crate::runtime::max_revision_id(doc) + 1;
    for_each_revision_id_mut(doc, &mut |id| {
        if *id == 0 {
            *id = next;
            next += 1;
        }
    });
}

/// Visit every tracked-change carrier's `revision_id` mutably, in the carrier
/// set `tracked_model::resolvable_revision_ids` defines. The single mutable
/// mirror of that read-only walk; used by [`mint_wire_zero_revision_ids`].
///
/// `pub(crate)` only so the drift-guard test
/// `tracked_model::tests::for_each_revision_id_mut_mirrors_resolvable_revision_ids`
/// can bind this walk to `resolvable_revision_ids` and fail loudly if the two
/// ever diverge (a carrier added to one but not the other).
pub(crate) fn for_each_revision_id_mut(doc: &mut CanonDoc, f: &mut dyn FnMut(&mut u32)) {
    fn visit_status(status: &mut TrackingStatus, f: &mut dyn FnMut(&mut u32)) {
        match status {
            TrackingStatus::Normal => {}
            TrackingStatus::Inserted(r) | TrackingStatus::Deleted(r) => f(&mut r.revision_id),
            TrackingStatus::InsertedThenDeleted(sr) => {
                f(&mut sr.inserted.revision_id);
                f(&mut sr.deleted.revision_id);
            }
        }
    }
    fn visit_optional_status(status: &mut Option<TrackingStatus>, f: &mut dyn FnMut(&mut u32)) {
        if let Some(s) = status {
            visit_status(s, f);
        }
    }
    fn visit_paragraph(p: &mut ParagraphNode, f: &mut dyn FnMut(&mut u32)) {
        if let Some(change) = &mut p.section_property_change {
            f(&mut change.revision.revision_id);
        }
        for seg in &mut p.segments {
            visit_status(&mut seg.status, f);
            for inline in &mut seg.inlines {
                match inline {
                    InlineNode::Text(t) => {
                        if let Some(fc) = &mut t.formatting_change {
                            f(&mut fc.revision_id);
                        }
                    }
                    InlineNode::OpaqueInline(opaque) => {
                        if let OpaqueKind::Hyperlink(data) = &mut opaque.kind {
                            for run in &mut data.runs {
                                visit_status(&mut run.status, f);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        visit_optional_status(&mut p.para_mark_status, f);
        if let Some(fc) = &mut p.formatting_change {
            f(&mut fc.revision_id);
        }
    }
    fn visit_block(block: &mut BlockNode, f: &mut dyn FnMut(&mut u32)) {
        match block {
            BlockNode::Paragraph(p) => visit_paragraph(p, f),
            BlockNode::Table(t) => {
                if let Some(fc) = &mut t.formatting_change {
                    f(&mut fc.revision_id);
                }
                for row in &mut t.rows {
                    visit_optional_status(&mut row.tracking_status, f);
                    if let Some(fc) = &mut row.formatting_change {
                        f(&mut fc.revision_id);
                    }
                    for cell in &mut row.cells {
                        visit_optional_status(&mut cell.tracking_status, f);
                        if let Some(fc) = &mut cell.formatting_change {
                            f(&mut fc.revision_id);
                        }
                        for nested in &mut cell.blocks {
                            visit_block(nested, f);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    fn visit_blocks(blocks: &mut [TrackedBlock], f: &mut dyn FnMut(&mut u32)) {
        for tb in blocks {
            visit_status(&mut tb.status, f);
            visit_block(&mut tb.block, f);
        }
    }
    visit_blocks(&mut doc.blocks, f);
    for story in &mut doc.headers {
        visit_blocks(&mut story.blocks, f);
    }
    for story in &mut doc.footers {
        visit_blocks(&mut story.blocks, f);
    }
    for story in &mut doc.footnotes {
        visit_blocks(&mut story.blocks, f);
    }
    for story in &mut doc.endnotes {
        visit_blocks(&mut story.blocks, f);
    }
    for story in &mut doc.comments {
        visit_optional_status(&mut story.tracking_status, f);
        visit_blocks(&mut story.blocks, f);
    }
    // The body-level `w:sectPrChange` lives outside `doc.blocks`.
    if let Some(change) = &mut doc.body_section_property_change {
        f(&mut change.revision.revision_id);
    }
}

/// Shared implementation for building a canonical document from a parsed DOCX archive.
fn build_canonical_from_archive(
    archive: &DocxArchive,
    fingerprint: DocFingerprint,
) -> Result<(CanonDoc, Vec<Diagnostic>), RuntimeError> {
    // Locate the main document part via the OPC officeDocument relationship
    // (ECMA-376 Part 2 §9.3) — its name is not fixed at word/document.xml.
    let main_part =
        crate::docx_package::resolve_main_document_part(archive).map_err(map_package_error)?;
    let main_dir = crate::docx_package::part_dir(&main_part);
    let document_xml = archive
        .get(&main_part)
        .ok_or_else(|| invalid_docx(&format!("main document part {main_part} is missing")))?;

    // Load numbering definitions (optional - may not exist in all docx files)
    let numbering_defs = parse_optional_docx_part(
        archive,
        "word/numbering.xml",
        crate::numbering::NumberingDefinitions::parse,
    )?;

    // Load style definitions (optional - may not exist in all docx files)
    let mut style_defs = parse_optional_docx_part(
        archive,
        "word/styles.xml",
        crate::styles::StyleDefinitions::parse,
    )?;

    // Load theme font definitions (optional) and attach to style definitions
    let theme_fonts = parse_optional_docx_part(
        archive,
        "word/theme/theme1.xml",
        crate::styles::ThemeFonts::parse,
    )?;
    if let (Some(theme_fonts), Some(ref mut sd)) = (theme_fonts, style_defs.as_mut()) {
        sd.set_theme_fonts(theme_fonts);
    }

    // Load default tab stop interval from settings.xml (default: 720 twips = 0.5 inch)
    let default_tab_stop = crate::settings::parse_default_tab_stop(archive)
        .map_err(invalid_docx_message)?
        .unwrap_or(720);

    // Parse compatibility settings from settings.xml (MS-DOCX §2.3)
    let compat_settings =
        crate::settings::parse_compat_settings(archive).map_err(invalid_docx_message)?;

    // Parse the three-state w:evenAndOddHeaders toggle (ISO 29500-1 §17.15.1.35):
    // None = absent, Some(true) = on, Some(false) = explicitly off. The
    // absent-vs-off distinction is carried honestly through to serialization.
    let even_and_odd_headers =
        crate::settings::parse_even_and_odd_headers_state(archive).map_err(invalid_docx_message)?;

    // Parse document relationships for stories
    let rels = parse_document_relationships(archive, &main_part)?;

    // Build rId → target lookup for resolving header/footer references in sectPr.
    let rel_lookup = build_rel_lookup_from_rels(&rels);

    // Stream document.xml one top-level block at a time (Rung 6). We never
    // materialize the whole body tree: each body child's subtree is built,
    // consumed into the block list AND scanned for sectPr header/footer refs,
    // then dropped before the next child. This bounds the transient tree to
    // O(one block) instead of O(whole document).
    let mut blocks = Vec::new();
    let mut diagnostics = Vec::new();
    let mut opaque_counter = 1u32;
    let mut inline_counter = 1u32;
    let mut table_counter = 1u32;
    let mut block_id_counter = 1u32;
    let mut numbering_state = crate::numbering::NumberingState::new();
    let mut ctx = ParseContext {
        diagnostics: &mut diagnostics,
        opaque_counter: &mut opaque_counter,
        inline_counter: &mut inline_counter,
        block_id_counter: &mut block_id_counter,
        numbering_defs: numbering_defs.as_ref(),
        numbering_state: &mut numbering_state,
        style_defs: style_defs.as_ref(),
        default_tab_stop,
        compat_settings: &compat_settings,
        rel_lookup: &rel_lookup,
        active_move_name: None,
        active_move_status: None,
    };
    let mut body_section_properties = None;
    let mut body_section_property_change = None;
    let mut header_refs: Vec<HeaderFooterRef> = Vec::new();
    let mut footer_refs: Vec<HeaderFooterRef> = Vec::new();

    // Seed the MCE scope from the document-root ancestors (w:document + w:body),
    // so an mc:Ignorable/mc:ProcessContent declared there governs body descendants
    // (ISO/IEC 29500-3 §9.2). The streaming body importer never materializes these
    // ancestors, so we extract them once (cheap — reads only the two outer tags).
    let mce_ancestors =
        word_xml::document_root_ancestors(document_xml).map_err(map_word_xml_error)?;
    let mce_seed =
        crate::word_ir::MceScope::from_ancestors(&mce_ancestors.iter().collect::<Vec<_>>())
            .map_err(map_word_ir_error)?;

    word_xml::for_each_body_child(document_xml, |index, element| {
        // Collect header/footer references from any sectPr within this block
        // (body-level final sectPr, or a section break inside w:p/w:pPr).
        collect_sect_pr_refs(element, &mut header_refs, &mut footer_refs)?;

        consume_body_child(
            element,
            index,
            &mut blocks,
            &mut table_counter,
            &mut ctx,
            &mut body_section_properties,
            &mut body_section_property_change,
            &rel_lookup,
            &mce_seed,
        )
    })
    .map_err(|e| match e {
        word_xml::BodyStreamError::Xml(xe) => map_word_xml_error(xe),
        word_xml::BodyStreamError::Consumer(re) => re,
    })?;

    // Match the dedup contract of `parse_header_footer_refs`: sort by rel_id and
    // dedup so multiple sections referencing the same part collapse to one.
    header_refs.sort_by(|a, b| a.rel_id.cmp(&b.rel_id));
    footer_refs.sort_by(|a, b| a.rel_id.cmp(&b.rel_id));
    header_refs.dedup_by(|a, b| a.rel_id == b.rel_id);
    footer_refs.dedup_by(|a, b| a.rel_id == b.rel_id);

    // Preserve every header/footer part explicitly referenced by sectPr.
    // `evenAndOddHeaders` controls which stories Word displays, not whether
    // the referenced story parts exist in the package.
    let headers = parse_headers(
        archive,
        &rels,
        &header_refs,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        main_dir,
        &mut diagnostics,
    )?;
    let footers = parse_footers(
        archive,
        &rels,
        &footer_refs,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        main_dir,
        &mut diagnostics,
    )?;
    let footnotes = parse_footnotes(
        archive,
        &rels,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        main_dir,
    )?;
    let endnotes = parse_endnotes(
        archive,
        &rels,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        main_dir,
    )?;
    let comments = parse_comments(
        archive,
        &rels,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        main_dir,
    )?;
    let comments_extended = parse_comments_extended(archive, &rels, main_dir)?;

    // `<w:background>` is a sibling of `<w:body>` (before it in CT_Document), so
    // the streaming body importer never sees it — materialize it separately.
    let document_background = word_xml::parse_document_background_element(document_xml)
        .map_err(map_word_xml_error)?
        .as_ref()
        .map(document_background_from_element);

    let mut doc = assemble_canonical_doc(AssembleCanonical {
        blocks,
        fingerprint,
        headers,
        footers,
        footnotes,
        endnotes,
        comments,
        body_section_properties,
        body_section_property_change,
        document_background,
    });

    doc.compat_settings = compat_settings;
    doc.comments_extended = comments_extended;
    doc.even_and_odd_headers = even_and_odd_headers;
    apply_document_protection(archive, &mut doc, &mut diagnostics).map_err(invalid_docx_message)?;

    // Resolve external hyperlink URLs from document relationships
    resolve_hyperlink_urls(&mut doc, &rels.hyperlinks);

    mint_wire_zero_revision_ids(&mut doc);
    ensure_unique_formatting_change_ids(&mut doc);

    Ok((doc, diagnostics))
}

/// Build a lookup from relationship IDs to their targets for all
/// header/footer relationships. Used to resolve rIds at the parse boundary.
pub(crate) fn build_rel_lookup_from_rels(rels: &DocumentRelationships) -> HashMap<String, String> {
    let mut lookup = HashMap::new();
    for rel in &rels.headers {
        lookup.insert(rel.id.clone(), rel.target.clone());
    }
    for rel in &rels.footers {
        lookup.insert(rel.id.clone(), rel.target.clone());
    }
    lookup
}

/// Build canonical document from root element with pre-parsed stories.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_canonical_from_root_with_stories(
    root: &Element,
    fingerprint: DocFingerprint,
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
    compat_settings: &CompatSettings,
    rel_lookup: &HashMap<String, String>,
    headers: Vec<HeaderStory>,
    footers: Vec<FooterStory>,
    footnotes: Vec<FootnoteStory>,
    endnotes: Vec<EndnoteStory>,
    comments: Vec<CommentStory>,
) -> Result<(CanonDoc, Vec<Diagnostic>), RuntimeError> {
    let body = body_element(root).map_err(map_word_xml_error)?;

    let mut blocks = Vec::new();
    let mut diagnostics = Vec::new();
    let mut opaque_counter = 1u32;
    let mut inline_counter = 1u32;
    let mut table_counter = 1u32;
    let mut block_id_counter = 1u32;
    let mut numbering_state = crate::numbering::NumberingState::new();
    let mut ctx = ParseContext {
        diagnostics: &mut diagnostics,
        opaque_counter: &mut opaque_counter,
        inline_counter: &mut inline_counter,
        block_id_counter: &mut block_id_counter,
        numbering_defs,
        numbering_state: &mut numbering_state,
        style_defs,
        default_tab_stop,
        compat_settings,
        rel_lookup,
        active_move_name: None,
        active_move_status: None,
    };

    // Seed the MCE scope from the document-root ancestors (w:document + w:body):
    // their mc:Ignorable/mc:ProcessContent govern body descendants (§9.2).
    // `from_ancestors` reads only attributes + namespace declarations, so passing
    // the full (child-bearing) elements is correct and cheap.
    let mce_seed =
        crate::word_ir::MceScope::from_ancestors(&[root, body]).map_err(map_word_ir_error)?;

    let mut body_section_properties = None;
    let mut body_section_property_change = None;
    for (index, child) in body.children.iter().enumerate() {
        let element = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        consume_body_child(
            element,
            index,
            &mut blocks,
            &mut table_counter,
            &mut ctx,
            &mut body_section_properties,
            &mut body_section_property_change,
            rel_lookup,
            &mce_seed,
        )?;
    }

    let document_background = parse_document_background(root);

    let mut doc = assemble_canonical_doc(AssembleCanonical {
        blocks,
        fingerprint,
        headers,
        footers,
        footnotes,
        endnotes,
        comments,
        body_section_properties,
        body_section_property_change,
        document_background,
    });

    mint_wire_zero_revision_ids(&mut doc);
    ensure_unique_formatting_change_ids(&mut doc);

    Ok((doc, diagnostics))
}

/// Find and parse `<w:background>` (ISO 29500-1 §17.2.1, CT_Background) from the
/// `<w:document>` root's children, if present.
///
/// `w:background` is a sibling of `w:body` (it precedes the body in
/// CT_Document), so the body-child loop never sees it — we read it directly
/// off the root's children here. Returns `None` when absent (absent != white).
fn parse_document_background(root: &Element) -> Option<crate::domain::DocumentBackground> {
    let bg = root.children.iter().find_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, "background") => Some(el),
        _ => None,
    })?;
    Some(document_background_from_element(bg))
}

/// Build a `DocumentBackground` from an already-materialized `<w:background>`
/// element. The four `w:*` attributes are carried verbatim; any child nodes
/// (the optional VML drawing) are serialized and preserved opaquely so the
/// subtree round-trips without silent loss.
fn document_background_from_element(bg: &Element) -> crate::domain::DocumentBackground {
    let drawing_xml = bg
        .children
        .iter()
        .map(|node| {
            let mut w = crate::xml_write::XmlWriter::new();
            // Children are preserved verbatim; serialization is infallible for
            // an already-parsed in-memory node.
            w.write_xml_node(node)
                .expect("serializing a parsed background child node cannot fail");
            String::from_utf8(w.into_inner()).expect("xmltree emits valid UTF-8 for a parsed node")
        })
        .collect();

    crate::domain::DocumentBackground {
        color: attr_get(bg, "w:color").cloned(),
        theme_color: attr_get(bg, "w:themeColor").cloned(),
        theme_tint: attr_get(bg, "w:themeTint").cloned(),
        theme_shade: attr_get(bg, "w:themeShade").cloned(),
        drawing_xml,
    }
}

/// Inputs to `assemble_canonical_doc`: the per-body block list plus the
/// pre-parsed stories and body-level section properties.
struct AssembleCanonical {
    blocks: Vec<TrackedBlock>,
    fingerprint: DocFingerprint,
    headers: Vec<HeaderStory>,
    footers: Vec<FooterStory>,
    footnotes: Vec<FootnoteStory>,
    endnotes: Vec<EndnoteStory>,
    comments: Vec<CommentStory>,
    body_section_properties: Option<crate::domain::SectionProperties>,
    body_section_property_change: Option<SectionPropertyChange>,
    document_background: Option<crate::domain::DocumentBackground>,
}

/// The `ST_DocProtect` XML token for a [`DocProtectEdit`], used in the enforced-
/// protection import diagnostic so its wording names the mode Word declared.
fn doc_protect_edit_token(mode: DocProtectEdit) -> &'static str {
    match mode {
        DocProtectEdit::None => "none",
        DocProtectEdit::ReadOnly => "readOnly",
        DocProtectEdit::Comments => "comments",
        DocProtectEdit::TrackedChanges => "trackedChanges",
        DocProtectEdit::Forms => "forms",
    }
}

/// Parse `w:documentProtection` from `archive` and record it on `doc`, emitting
/// one import [`Diagnostic`] when the declaration is ENFORCED.
///
/// The engine reports protection but does not honor it: a document declaring
/// enforced protection is imported normally (never refused), and edits authored
/// here ignore the restriction — so an enforced declaration gets an observable
/// diagnostic naming the edit mode. Shared by both import paths (the whole-tree
/// builder and the anchor path) so the declaration and its diagnostic wording
/// have a single source. See [`crate::domain::DocumentProtection`].
pub(crate) fn apply_document_protection(
    archive: &crate::docx::DocxArchive,
    doc: &mut CanonDoc,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), String> {
    let protection = crate::settings::parse_document_protection(archive)?;
    if let Some(p) = &protection
        && p.enforcement == Some(true)
    {
        let edit_mode = p.edit.map(doc_protect_edit_token).unwrap_or("unspecified");
        diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Warning,
            message: format!(
                "document declares enforced protection (edit={edit_mode}); \
                 engine edits do not honor protection"
            ),
            context: Some("w:documentProtection".to_string()),
        });
    }
    doc.document_protection = protection;
    Ok(())
}

/// Assemble the final `CanonDoc` from built blocks + stories and run the
/// section-inheritance / header-footer post-passes. Shared by the whole-tree
/// builder and the streaming archive builder so both produce identical docs.
fn assemble_canonical_doc(parts: AssembleCanonical) -> CanonDoc {
    let mut doc = CanonDoc {
        id: NodeId::from("doc"),
        blocks: parts.blocks,
        meta: DocMeta {
            schema_version: SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: parts.fingerprint,
            internal_ids_version: INTERNAL_IDS_VERSION_V0.to_string(),
        },
        headers: parts.headers,
        footers: parts.footers,
        footnotes: parts.footnotes,
        endnotes: parts.endnotes,
        comments: parts.comments,
        comments_extended: vec![],
        body_section_properties: parts.body_section_properties,
        body_section_property_change: parts.body_section_property_change,
        compat_settings: CompatSettings::default(),
        // Populated by the archive-aware caller (it has settings.xml); default
        // to absent here so the root builder stays archive-agnostic.
        even_and_odd_headers: None,
        document_background: parts.document_background,
        // Populated by the archive-aware caller (it has settings.xml); default
        // to absent here so the root builder stays archive-agnostic.
        document_protection: None,
    };

    propagate_continuous_section_properties(&mut doc);
    resolve_section_header_inheritance(&mut doc);
    synthesize_blank_headers_for_first_section(&mut doc);
    synthesize_blank_footers_for_first_section(&mut doc);

    // H2: one unified body-state validator after the import (post-parse)
    // producer. NOTE: import is byte-faithful over arbitrary — possibly
    // non-conformant — input, so the debug-assert here uses the IMPORT scope,
    // which checks only the invariants import cannot construct a violation of
    // (mark-suppression, stacked-state). The final-mark rule and table coherence
    // are POST-TRANSFORM properties established by the mint-time normalizers, not
    // by import; a wild document may legitimately carry a tracked final pilcrow
    // or a non-conformant cell mark, and those are REPORTED (not panicked) by the
    // public `api::validate` surface instead.
    crate::tracked_model::debug_assert_import_body_invariants(&doc, "import");

    doc
}

/// Consume one direct child of `<w:body>` into the canonical block list.
///
/// This is the per-child body-loop body, extracted so that both the whole-tree
/// path (`build_canonical_from_root_with_stories`) and the streaming, one-block-
/// at-a-time path (`build_canonical_from_archive_streaming`) drive the SAME
/// logic. `index` is the child's position among `body.children`, used as the
/// `body_index` anchor for `append_blocks_from_element`.
///
/// State that spans children (`active_move_*`) lives on `ctx`; body-level section
/// properties are written into the two out-params.
#[allow(clippy::too_many_arguments)]
fn consume_body_child(
    element: &Element,
    index: usize,
    blocks: &mut Vec<TrackedBlock>,
    table_counter: &mut u32,
    ctx: &mut ParseContext<'_>,
    body_section_properties: &mut Option<crate::domain::SectionProperties>,
    body_section_property_change: &mut Option<SectionPropertyChange>,
    rel_lookup: &HashMap<String, String>,
    mce_seed: &crate::word_ir::MceScope,
) -> Result<(), RuntimeError> {
    // MCE Step-1 (ISO/IEC 29500-3 §9.2/§9.4): drop ignored elements with their
    // contents, unwrap ProcessContent matches, and refuse unsupported
    // MustUnderstand — on the CONSUMPTION (model) path, before block extraction
    // sees the subtree. `mce_seed` carries the ancestor declarations
    // (w:document + w:body), so §9.2's "this element or an ANCESTOR" is honored
    // even when the mc:Ignorable/mc:ProcessContent lives on the document root and
    // the foreign element below carries no mc:* attribute. The transform runs
    // (and clones) only when the subtree has a local directive OR an ancestor
    // directive is in force AND the subtree contains a foreign-namespace element —
    // so a pure-WML body child stays clone-free even under a document-root
    // mc:Ignorable. A whole body child that resolves to "ignored" contributes no
    // block.
    let needs_transform = crate::word_ir::subtree_has_mce_directives(element)
        || (mce_seed.has_directives()
            && crate::word_ir::subtree_has_foreign_namespace_element(element));
    let mce_owned;
    let element = if needs_transform {
        match crate::word_ir::mce_preprocess_element(element, mce_seed)
            .map_err(map_word_ir_error)?
        {
            Some(transformed) => {
                mce_owned = transformed;
                &mce_owned
            }
            None => return Ok(()),
        }
    } else {
        element
    };

    // Compat-tolerance edge (see `crate::compat`): rewrite schema-invalid but
    // Word-accepted within-subtree shapes (w:shd w:val="none"; nested w:r) into
    // their valid equivalents BEFORE block extraction, recording a diagnostic
    // for each. The `w:tbl`-as-child-of-`w:p` hoist is structural and handled in
    // `append_blocks_from_element`. Gated on a read-only pre-check so conformant
    // body children are never cloned.
    let compat_owned;
    let element = if crate::compat::subtree_has_tolerated_shape(element) {
        compat_owned = crate::compat::normalize_tolerated_shapes(element, ctx.diagnostics);
        &compat_owned
    } else {
        element
    };

    // Extract sectPrChange from body-level w:sectPr (final section properties).
    if is_w_tag(element, "sectPr") {
        *body_section_properties = Some(crate::word_ir::parse_section_properties(
            element, rel_lookup,
        ));
        *body_section_property_change = extract_body_section_property_change(element)?;
        return Ok(());
    }

    // Track move range boundaries (ECMA-376 §17.13.5.23-28).
    // moveFromRangeStart/moveToRangeStart carry w:name that links a move source
    // to its destination. We capture the name and tracking status so that:
    // 1. Body-level moveFrom/moveTo containers get move_id from active_move_name
    // 2. Paragraphs between range markers (our serializer's format) get tagged too
    if is_w_tag(element, "moveFromRangeStart") {
        if let Some(name) = attr_get(element, "name") {
            ctx.active_move_name = Some(name.clone());
            let revision_id = parse_revision_id(element, "w:moveFromRangeStart")?;
            let author = attr_get(element, "author").cloned();
            let date = attr_get(element, "date").cloned();
            ctx.active_move_status = Some(TrackingStatus::Deleted(RevisionInfo {
                revision_id,
                author,
                date,
                apply_op_id: None,
            }));
        }
        return Ok(());
    }
    if is_w_tag(element, "moveToRangeStart") {
        if let Some(name) = attr_get(element, "name") {
            ctx.active_move_name = Some(name.clone());
            let revision_id = parse_revision_id(element, "w:moveToRangeStart")?;
            let author = attr_get(element, "author").cloned();
            let date = attr_get(element, "date").cloned();
            ctx.active_move_status = Some(TrackingStatus::Inserted(RevisionInfo {
                revision_id,
                author,
                date,
                apply_op_id: None,
            }));
        }
        return Ok(());
    }
    if is_w_tag(element, "moveFromRangeEnd") || is_w_tag(element, "moveToRangeEnd") {
        ctx.active_move_name = None;
        ctx.active_move_status = None;
        return Ok(());
    }

    let before_len = blocks.len();
    append_blocks_from_element(element, Some(index), blocks, table_counter, ctx)?;

    // Tag blocks parsed within an active move range. This handles our
    // serializer's output format where range markers wrap a normal w:p
    // (with moveFrom/moveTo inside the paragraph at run level).
    if let Some(ref move_name) = ctx.active_move_name {
        for tb in &mut blocks[before_len..] {
            if tb.move_id.is_none() {
                tb.move_id = Some(move_name.clone());
            }
            if matches!(tb.status, TrackingStatus::Normal)
                && let Some(ref status) = ctx.active_move_status
            {
                tb.status = status.clone();
                if let BlockNode::Paragraph(p) = &mut tb.block {
                    p.para_mark_status = Some(status.clone());
                }
            }
        }
    }
    Ok(())
}

/// ISO 29500-1 §17.6.17: A continuous section break does not start a new page.
/// When a continuous section omits page-level properties (margins, page size, etc.),
/// those properties are inherited from the previous section.
///
/// This function performs a forward pass over all sections in document order
/// (mid-document sectPr from paragraphs, then body-level sectPr) and fills in
/// None margin fields on continuous sections from the preceding section.
fn propagate_continuous_section_properties(doc: &mut CanonDoc) {
    let para_indices: Vec<usize> = doc
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(i, tb)| {
            if let BlockNode::Paragraph(p) = &tb.block
                && p.section_properties.is_some()
            {
                return Some(i);
            }
            None
        })
        .collect();

    let mut prev_props: Option<crate::domain::SectionProperties> = None;

    for &idx in &para_indices {
        if let BlockNode::Paragraph(ref mut p) = doc.blocks[idx].block
            && let Some(ref mut sp) = p.section_properties
        {
            if sp.section_type == Some(SectionType::Continuous)
                && let Some(ref prev) = prev_props
            {
                inherit_page_properties(sp, prev);
            }
            prev_props = Some(sp.clone());
        }
    }

    if let Some(ref mut body_sp) = doc.body_section_properties
        && body_sp.section_type == Some(SectionType::Continuous)
        && let Some(ref prev) = prev_props
    {
        inherit_page_properties(body_sp, prev);
    }
}

/// Walk sections in document order and resolve header/footer inheritance.
///
/// Per ISO 29500-1 §17.10.2: when a section omits a headerReference for a
/// given kind, it inherits that kind from the preceding section. Inheritance
/// is per-kind — a section can declare "first" while inheriting "default".
///
/// Also filters out refs whose rel_id does not match any story in
/// `doc.headers` / `doc.footers` (e.g., Even headers filtered by
/// `filter_even_headers_footers`).
fn resolve_section_header_inheritance(doc: &mut CanonDoc) {
    use crate::domain::StoryRef;

    let valid_header_ids: HashSet<String> =
        doc.headers.iter().map(|h| h.part_name.clone()).collect();
    let valid_footer_ids: HashSet<String> =
        doc.footers.iter().map(|f| f.part_name.clone()).collect();

    let para_indices: Vec<usize> = doc
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(i, tb)| {
            if let BlockNode::Paragraph(p) = &tb.block
                && p.section_properties.is_some()
            {
                return Some(i);
            }
            None
        })
        .collect();

    let mut prev_headers: Vec<StoryRef> = Vec::new();
    let mut prev_footers: Vec<StoryRef> = Vec::new();

    for &idx in &para_indices {
        if let BlockNode::Paragraph(ref mut p) = doc.blocks[idx].block
            && let Some(ref mut sp) = p.section_properties
        {
            resolve_refs(&mut sp.header_refs, &prev_headers, &valid_header_ids);
            resolve_refs(&mut sp.footer_refs, &prev_footers, &valid_footer_ids);
            prev_headers = sp.header_refs.clone();
            prev_footers = sp.footer_refs.clone();
        }
    }

    // Body-level section is always last
    if let Some(ref mut body_sp) = doc.body_section_properties {
        resolve_refs(&mut body_sp.header_refs, &prev_headers, &valid_header_ids);
        resolve_refs(&mut body_sp.footer_refs, &prev_footers, &valid_footer_ids);
    }
}

/// Merge declared refs with inherited refs (per-kind override).
/// Filter out any refs pointing to stories not in `valid_ids`.
fn resolve_refs(
    declared: &mut Vec<crate::domain::StoryRef>,
    inherited: &[crate::domain::StoryRef],
    valid_ids: &HashSet<String>,
) {
    // Filter declared to only valid refs
    declared.retain(|r| valid_ids.contains(&r.part_path));

    // For each inherited kind not already declared, inherit — marked
    // synthesized so the serializer doesn't materialize §17.10.2 inheritance
    // as direct markup.
    for inh in inherited {
        if !declared.iter().any(|d| d.kind == inh.kind) {
            let mut inherited_ref = inh.clone();
            inherited_ref.synthesized = true;
            declared.push(inherited_ref);
        }
    }
}

/// Per ECMA-376 §17.10.2 / ISO 29500-1 §17.10.5: when the first section of a
/// document has no headerReference for the Default kind (and there is no
/// preceding section to inherit from), a blank/empty default header is
/// synthesized.
///
/// Additionally, per §17.10.5: when the first section has titlePg=true but no
/// first-page headerReference, a blank first-page header is synthesized (since
/// there is no preceding section to inherit from).
///
/// This must run after `resolve_section_header_inheritance` so that
/// inheritance has already been resolved and we only fill in gaps for
/// the first section.
pub(crate) fn synthesize_blank_headers_for_first_section(doc: &mut CanonDoc) {
    use crate::domain::StoryRef;

    // Find the first section's header_refs. In document order, mid-document
    // sectPr paragraphs come before the body-level sectPr.
    let first_section_is_body = doc.blocks.iter().all(|tb| {
        if let BlockNode::Paragraph(p) = &tb.block {
            p.section_properties.is_none()
        } else {
            true
        }
    });

    // Determine which header kinds need synthesis
    let (needs_blank_default, needs_blank_first) = if first_section_is_body {
        doc.body_section_properties
            .as_ref()
            .map(|sp| {
                let has_default = sp
                    .header_refs
                    .iter()
                    .any(|r| r.kind == HeaderFooterKind::Default);
                let has_first = sp
                    .header_refs
                    .iter()
                    .any(|r| r.kind == HeaderFooterKind::First);
                let title_pg = sp.title_page == Some(true);
                (!has_default, title_pg && !has_first)
            })
            .unwrap_or((false, false))
    } else {
        doc.blocks
            .iter()
            .find_map(|tb| {
                if let BlockNode::Paragraph(p) = &tb.block {
                    p.section_properties.as_ref()
                } else {
                    None
                }
            })
            .map(|sp| {
                let has_default = sp
                    .header_refs
                    .iter()
                    .any(|r| r.kind == HeaderFooterKind::Default);
                let has_first = sp
                    .header_refs
                    .iter()
                    .any(|r| r.kind == HeaderFooterKind::First);
                let title_pg = sp.title_page == Some(true);
                (!has_default, title_pg && !has_first)
            })
            .unwrap_or((false, false))
    };

    if !needs_blank_default && !needs_blank_first {
        return;
    }

    // Synthesize blank header stories for each needed kind.
    let kinds_to_synthesize: Vec<(HeaderFooterKind, &str)> = [
        (
            needs_blank_default,
            HeaderFooterKind::Default,
            "synthesized-blank-header-default",
        ),
        (
            needs_blank_first,
            HeaderFooterKind::First,
            "synthesized-blank-header-first",
        ),
    ]
    .iter()
    .filter(|(needed, _, _)| *needed)
    .map(|(_, kind, name)| (kind.clone(), *name))
    .collect();

    for (kind, name_prefix) in &kinds_to_synthesize {
        let part_suffix = if *kind == HeaderFooterKind::First {
            "first"
        } else {
            "default"
        };
        let part_name = format!("synthesized-blank-header-{part_suffix}.xml");
        let empty_para = synthesize_blank_paragraph(name_prefix, "Header");
        let content_hash = compute_story_content_hash(std::slice::from_ref(&empty_para));

        doc.headers.push(HeaderStory {
            part_name: part_name.clone(),
            kind: kind.clone(),
            blocks: vec![normal_tracked_block(empty_para)],
            content_hash,
            synthesized: true,
        });

        let story_ref = StoryRef {
            kind: kind.clone(),
            part_path: part_name,
            // §17.10.5 blank synthesis — render semantics, never markup.
            synthesized: true,
        };

        if first_section_is_body {
            if let Some(ref mut sp) = doc.body_section_properties {
                sp.header_refs.push(story_ref);
            }
        } else {
            for tb in doc.blocks.iter_mut() {
                if let BlockNode::Paragraph(ref mut p) = tb.block
                    && let Some(ref mut sp) = p.section_properties
                {
                    sp.header_refs.push(story_ref);
                    break;
                }
            }
        }
    }
}

/// Synthesize a blank paragraph for use in blank header/footer stories.
fn synthesize_blank_paragraph(rel_id: &str, style_id: &str) -> BlockNode {
    BlockNode::from(ParagraphNode {
        id: NodeId::from(format!("{rel_id}:p0")),
        style_id: Some(style_id.to_string().into()),
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
        segments: vec![],
        block_text_hash: None,
        numbering: None,
        has_direct_numbering: true,
        numbering_suppressed: false,
        materialized_numbering: None,
        rendered_text: None,
        literal_prefix: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: RunRprAuthored::default(),
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
        text_direction: None,
        suppress_auto_hyphens: None,
        snap_to_grid: None,
        overflow_punct: None,
        adjust_right_ind: None,
        word_wrap: None,
        frame_pr: None,
        para_id: None,
        text_id: None,
        cnf_style: None,
        preserved_ppr: Vec::new(),
    })
}

/// Per ECMA-376 §17.10.2 / ISO 29500-1 §17.10.5: when the first section of a
/// document has no footerReference for the Default kind (and there is no
/// preceding section to inherit from), a blank/empty default footer is
/// synthesized.
///
/// Additionally, per §17.10.5: when the first section has titlePg=true but no
/// first-page footerReference, a blank first-page footer is synthesized.
///
/// This mirrors `synthesize_blank_headers_for_first_section` exactly.
pub(crate) fn synthesize_blank_footers_for_first_section(doc: &mut CanonDoc) {
    use crate::domain::StoryRef;

    let first_section_is_body = doc.blocks.iter().all(|tb| {
        if let BlockNode::Paragraph(p) = &tb.block {
            p.section_properties.is_none()
        } else {
            true
        }
    });

    let (needs_blank_default, needs_blank_first) = if first_section_is_body {
        doc.body_section_properties
            .as_ref()
            .map(|sp| {
                let has_default = sp
                    .footer_refs
                    .iter()
                    .any(|r| r.kind == HeaderFooterKind::Default);
                let has_first = sp
                    .footer_refs
                    .iter()
                    .any(|r| r.kind == HeaderFooterKind::First);
                let title_pg = sp.title_page == Some(true);
                (!has_default, title_pg && !has_first)
            })
            .unwrap_or((false, false))
    } else {
        doc.blocks
            .iter()
            .find_map(|tb| {
                if let BlockNode::Paragraph(p) = &tb.block {
                    p.section_properties.as_ref()
                } else {
                    None
                }
            })
            .map(|sp| {
                let has_default = sp
                    .footer_refs
                    .iter()
                    .any(|r| r.kind == HeaderFooterKind::Default);
                let has_first = sp
                    .footer_refs
                    .iter()
                    .any(|r| r.kind == HeaderFooterKind::First);
                let title_pg = sp.title_page == Some(true);
                (!has_default, title_pg && !has_first)
            })
            .unwrap_or((false, false))
    };

    if !needs_blank_default && !needs_blank_first {
        return;
    }

    let kinds_to_synthesize: Vec<(HeaderFooterKind, &str)> = [
        (
            needs_blank_default,
            HeaderFooterKind::Default,
            "synthesized-blank-footer-default",
        ),
        (
            needs_blank_first,
            HeaderFooterKind::First,
            "synthesized-blank-footer-first",
        ),
    ]
    .iter()
    .filter(|(needed, _, _)| *needed)
    .map(|(_, kind, name)| (kind.clone(), *name))
    .collect();

    for (kind, name_prefix) in &kinds_to_synthesize {
        let part_suffix = if *kind == HeaderFooterKind::First {
            "first"
        } else {
            "default"
        };
        let part_name = format!("synthesized-blank-footer-{part_suffix}.xml");
        let empty_para = synthesize_blank_paragraph(name_prefix, "Footer");
        let content_hash = compute_story_content_hash(std::slice::from_ref(&empty_para));

        doc.footers.push(FooterStory {
            part_name: part_name.clone(),
            kind: kind.clone(),
            blocks: vec![normal_tracked_block(empty_para)],
            content_hash,
            synthesized: true,
        });

        let story_ref = StoryRef {
            kind: kind.clone(),
            part_path: part_name,
            // §17.10.5 blank synthesis — render semantics, never markup.
            synthesized: true,
        };

        if first_section_is_body {
            if let Some(ref mut sp) = doc.body_section_properties {
                sp.footer_refs.push(story_ref);
            }
        } else {
            for tb in doc.blocks.iter_mut() {
                if let BlockNode::Paragraph(ref mut p) = tb.block
                    && let Some(ref mut sp) = p.section_properties
                {
                    sp.footer_refs.push(story_ref);
                    break;
                }
            }
        }
    }
}

fn inherit_page_properties(
    target: &mut crate::domain::SectionProperties,
    source: &crate::domain::SectionProperties,
) {
    if target.margin_top.is_none() {
        target.margin_top = source.margin_top;
    }
    if target.margin_bottom.is_none() {
        target.margin_bottom = source.margin_bottom;
    }
    if target.margin_left.is_none() {
        target.margin_left = source.margin_left;
    }
    if target.margin_right.is_none() {
        target.margin_right = source.margin_right;
    }
    if target.header_distance.is_none() {
        target.header_distance = source.header_distance;
    }
    if target.footer_distance.is_none() {
        target.footer_distance = source.footer_distance;
    }
    if target.gutter.is_none() {
        target.gutter = source.gutter;
    }
    if target.page_width.is_none() {
        target.page_width = source.page_width;
    }
    if target.page_height.is_none() {
        target.page_height = source.page_height;
    }
    if target.orientation.is_none() {
        target.orientation = source.orientation.clone();
    }
}

struct ParseContext<'a> {
    diagnostics: &'a mut Vec<Diagnostic>,
    opaque_counter: &'a mut u32,
    inline_counter: &'a mut u32,
    /// Counter for body paragraph block IDs. Starts at 1; each paragraph
    /// claims `p_{counter}` and increments. Replaces the older
    /// anchor-bookmark mechanism that round-tripped IDs through XML.
    block_id_counter: &'a mut u32,
    numbering_defs: Option<&'a crate::numbering::NumberingDefinitions>,
    numbering_state: &'a mut crate::numbering::NumberingState,
    style_defs: Option<&'a crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
    compat_settings: &'a CompatSettings,
    /// rId → target lookup for resolving header/footer refs in sectPr.
    rel_lookup: &'a HashMap<String, String>,
    /// Active move name from a moveFromRangeStart/moveToRangeStart marker.
    /// Set when iterating body children inside a move range, used to populate
    /// TrackedBlock.move_id for moveFrom/moveTo containers and for blocks
    /// between range markers (our serializer's output format).
    active_move_name: Option<String>,
    /// Tracking status for blocks inside an active move range.
    /// Deleted for moveFrom ranges, Inserted for moveTo ranges.
    /// Extracted from the range start marker's w:author/w:date attributes.
    active_move_status: Option<TrackingStatus>,
}
/// What a nested-tracked-container scan found in a body item's subtree.
enum NestedTracking {
    /// `ins`-in-`ins` / `del`-in-`del` (at any nesting distance): invalid
    /// OOXML (validator rule I-TC-003, ISO 29500 §17.13.5) — refuse the
    /// document at import.
    SameType { name: String },
    /// An unsupported nested mix: a move container nested with anything, or
    /// nesting deeper than the one `ins`/`del` level the IR models —
    /// quarantine the body item. The SUPPORTED one-level `ins`/`del` pair
    /// (either order — the stacked state) is NOT reported: it parses into
    /// `TrackingStatus::InsertedThenDeleted`.
    UnsupportedMix { outer: String, inner: String },
}

/// True for the tracked-change CONTENT containers (run-bearing envelopes).
fn is_tracked_content_container(el: &Element) -> bool {
    is_w_tag(el, "ins") || is_w_tag(el, "del") || is_w_tag(el, "moveFrom") || is_w_tag(el, "moveTo")
}

/// Scan a body item's subtree for UNSUPPORTED tracked-container nesting.
///
/// Property bags are skipped: paragraph-mark and property-change markers
/// (`w:pPr/w:rPr/w:del`, `w:rPrChange`, `w:trPr/w:del`, math `ctrlPr`, …)
/// carry revision ELEMENTS that are not content containers. The skip rule is
/// grammar-shaped rather than a name list: any element whose local name ends
/// in `Pr` or `PrChange` is a property bag, and tracked content never lives
/// inside one.
///
/// `enclosing` is the chain of tracked-container names above this element.
fn find_nested_tracking(element: &Element, enclosing: &[&str]) -> Option<NestedTracking> {
    let name = element.name.as_str();
    if name.ends_with("Pr") || name.ends_with("PrChange") {
        return None;
    }
    // A textbox's content (w:txbxContent, inside a drawing/VML shape) is a
    // SEPARATE STORY: tracked-change nesting rules apply per story, so an
    // <w:ins> inside a textbox inside an inserted run is legal OOXML, not
    // I-TC-003 nesting. Drawings are opaque widgets — their inner story never
    // reaches the atom layer — so the scan must not cross the boundary either.
    if name == "txbxContent" {
        return None;
    }
    let mut chain: Vec<&str> = enclosing.to_vec();
    if is_tracked_content_container(element) {
        // Same-type at any nesting distance is invalid OOXML.
        if enclosing.contains(&name) {
            return Some(NestedTracking::SameType {
                name: name.to_string(),
            });
        }
        if let Some(outer) = enclosing.last() {
            let pair_supported =
                (*outer == "ins" && name == "del") || (*outer == "del" && name == "ins");
            // Deeper than one level, or a move-container mix: not modeled.
            if enclosing.len() >= 2 || !pair_supported {
                return Some(NestedTracking::UnsupportedMix {
                    outer: outer.to_string(),
                    inner: name.to_string(),
                });
            }
        }
        chain.push(name);
    }
    for child in &element.children {
        if let XMLNode::Element(el) = child
            && let Some(found) = find_nested_tracking(el, &chain)
        {
            return Some(found);
        }
    }
    None
}

fn append_blocks_from_element(
    element: &Element,
    body_index: Option<usize>,
    blocks: &mut Vec<TrackedBlock>,
    table_counter: &mut u32,
    ctx: &mut ParseContext,
) -> Result<(), RuntimeError> {
    // Nested-tracking quarantine: a body item
    // containing a tracked container nested inside another is either invalid
    // OOXML (same-type — refuse at the entry door, mirroring I-TC-003) or the
    // not-yet-representable stacked state (del-in-ins — quarantine the whole
    // body item as a byte-faithful opaque block). Both replace the old
    // behavior, which silently DROPPED the inner revision at atom extraction.
    // Scanned only at real body children (body_index present); recursion into
    // containers passes None, so each item is scanned exactly once.
    if let Some(index) = body_index {
        match find_nested_tracking(element, &[]) {
            None => {}
            Some(NestedTracking::SameType { name }) => {
                return Err(RuntimeError {
                    code: ErrorCode::InvalidDocx,
                    message: format!(
                        "invalid nested tracked change at body item {index}: <w:{name}> \
                         inside <w:{name}> (same-type nesting violates I-TC-003 / \
                         ISO 29500 §17.13.5)"
                    ),
                    details: ErrorDetails::default(),
                });
            }
            Some(NestedTracking::UnsupportedMix { outer, inner }) => {
                let opaque_id = NodeId::from(format!("opaque_{}", *ctx.opaque_counter));
                *ctx.opaque_counter += 1;
                let proof_ref = ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: opaque_id.clone(),
                    docx_anchor: format!("body_index:{index}"),
                };
                ctx.diagnostics.push(Diagnostic {
                    level: DiagnosticLevel::Warning,
                    message: format!(
                        "quarantined body item {index}: <w:{inner}> nested inside \
                         <w:{outer}> (stacked revisions not representable yet; \
                         preserved verbatim, uneditable)"
                    ),
                    context: Some(format!("body_index={index}")),
                });
                blocks.push(normal_tracked_block(BlockNode::from(OpaqueBlockNode {
                    id: opaque_id,
                    kind: OpaqueKind::QuarantinedNestedTracking,
                    opaque_ref: format!("body_item_{index}"),
                    proof_ref,
                    range_marker: None,
                })));
                return Ok(());
            }
        }
    }

    // Detect w:tbl explicitly FIRST, before element_contains_paragraph check
    // This prevents table flattening and builds proper TableNode structures
    if is_w_tag(element, "tbl") {
        let table = table_from_element(element, table_counter, ctx)?;
        blocks.push(normal_tracked_block(BlockNode::from(table)));
        return Ok(());
    }

    if is_w_tag(element, "p") {
        // Compat-tolerance edge (see `crate::compat`): a `w:tbl` as a direct
        // child of `w:p` is schema-invalid (CT_P has no tbl child) but Word
        // opens it without repair, rendering the table as block content. Hoist
        // each such table to a block-level sibling immediately AFTER the host
        // paragraph, preserving the paragraph's remaining children in order.
        // Done here, next to block assembly, because the rewrite is structural
        // (one body child becomes several blocks); the roundtrip therefore
        // differs from the invalid original by design.
        if crate::compat::paragraph_has_direct_table(element) {
            let context = match body_index {
                Some(index) => format!("body_index={index}"),
                None => "nested block".to_string(),
            };
            ctx.diagnostics
                .push(crate::compat::tbl_in_paragraph_diagnostic(context));
            let (paragraph_without_tables, hoisted_tables) =
                crate::compat::split_paragraph_tables(element);
            let block = paragraph_from_element(
                &paragraph_without_tables,
                ctx.inline_counter,
                ctx.block_id_counter,
                ctx.numbering_defs,
                ctx.numbering_state,
                ctx.style_defs,
                ctx.default_tab_stop,
                ctx.rel_lookup,
            )?;
            blocks.push(normal_tracked_block(block));
            for table_element in &hoisted_tables {
                let table = table_from_element(table_element, table_counter, ctx)?;
                blocks.push(normal_tracked_block(BlockNode::from(table)));
            }
            return Ok(());
        }

        let block = paragraph_from_element(
            element,
            ctx.inline_counter,
            ctx.block_id_counter,
            ctx.numbering_defs,
            ctx.numbering_state,
            ctx.style_defs,
            ctx.default_tab_stop,
            ctx.rel_lookup,
        )?;
        blocks.push(normal_tracked_block(block));
        return Ok(());
    }

    // Body-level w:ins/w:del — tracked insertion/deletion of entire blocks.
    // Must be checked BEFORE element_contains_paragraph, which would recurse
    // into children and lose the tracking context.
    if is_w_tag(element, "ins") || is_w_tag(element, "del") {
        let tracking_status = extract_block_tracking_status(element)?;
        let before_len = blocks.len();
        for child in &element.children {
            if let XMLNode::Element(el) = child {
                append_blocks_from_element(el, None, blocks, table_counter, ctx)?;
            }
        }
        // Tag all newly added blocks with the container's tracking status
        for tb in &mut blocks[before_len..] {
            tb.status = tracking_status.clone();
            if let BlockNode::Paragraph(p) = &mut tb.block {
                p.para_mark_status = Some(tracking_status.clone());
            }
        }
        return Ok(());
    }

    // Body-level w:moveFrom/w:moveTo — tracked move of entire blocks (ECMA-376 §17.13.5.21-26).
    // moveFrom = content moved away (semantically a deletion), moveTo = content moved here
    // (semantically an insertion). The active_move_name from the enclosing range markers
    // links the source and destination of the same move operation.
    if is_w_tag(element, "moveFrom") || is_w_tag(element, "moveTo") {
        let tracking_status = extract_block_tracking_status(element)?;
        let move_name = ctx.active_move_name.clone();
        let before_len = blocks.len();
        for child in &element.children {
            if let XMLNode::Element(el) = child {
                append_blocks_from_element(el, None, blocks, table_counter, ctx)?;
            }
        }
        for tb in &mut blocks[before_len..] {
            tb.status = tracking_status.clone();
            tb.move_id = move_name.clone();
            if let BlockNode::Paragraph(p) = &mut tb.block {
                p.para_mark_status = Some(tracking_status.clone());
            }
        }
        return Ok(());
    }

    // MC AlternateContent at body/table-cell level — select branch and recurse
    // into its children. Must be checked BEFORE element_contains_paragraph, which
    // would recurse into ALL branches and double-process content.
    if is_mc_alternate_content(element) {
        if let Some(branch) = select_mc_branch(element).map_err(map_word_ir_error)? {
            for child in &branch.children {
                if let XMLNode::Element(el) = child {
                    append_blocks_from_element(el, None, blocks, table_counter, ctx)?;
                }
            }
        }
        return Ok(());
    }

    // Body-level w:sdt — preserve as opaque block to keep the content control wrapper.
    // Must be checked BEFORE element_contains_paragraph, which would recurse into
    // sdtContent children and strip the SDT wrapper (losing form fields, dropdowns, etc.).
    // Only treated as opaque when at body level (body_index is Some); inside table cells
    // the SDT wrapper is preserved separately via SdtWrapper on the cell node.
    if is_w_tag(element, "sdt")
        && let Some(index) = body_index
    {
        let opaque_id = NodeId::from(format!("opaque_{}", *ctx.opaque_counter));
        *ctx.opaque_counter += 1;
        let proof_ref = ProofRef {
            part: DocPart::DocumentXml,
            block_id: opaque_id.clone(),
            docx_anchor: format!("body_index:{index}"),
        };
        ctx.diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Warning,
            message: format!("opaque block: {}", element.name),
            context: Some(format!("body_index={index}")),
        });
        blocks.push(normal_tracked_block(BlockNode::from(OpaqueBlockNode {
            id: opaque_id,
            kind: OpaqueKind::Sdt,
            opaque_ref: format!("body_item_{index}"),
            proof_ref,
            range_marker: None,
        })));
        return Ok(());
    }
    // Inside table cell — fall through to element_contains_paragraph
    // which will recurse into sdtContent children.

    if element_contains_paragraph(element) {
        for child in &element.children {
            if let XMLNode::Element(el) = child {
                append_blocks_from_element(el, None, blocks, table_counter, ctx)?;
            }
        }
        return Ok(());
    }

    if let Some(index) = body_index {
        let opaque_id = NodeId::from(format!("opaque_{}", *ctx.opaque_counter));
        *ctx.opaque_counter += 1;
        let kind = OpaqueKind::Unknown(element.name.clone());
        let proof_ref = ProofRef {
            part: DocPart::DocumentXml,
            block_id: opaque_id.clone(),
            docx_anchor: format!("body_index:{index}"),
        };
        ctx.diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Warning,
            message: format!("opaque block: {}", element.name),
            context: Some(format!("body_index={index}")),
        });
        // A body-level range-marker half (bookmark / comment-range / permission
        // between paragraphs) stays a verbatim-spliced opaque block — byte
        // fidelity is unchanged — but records its family/id/role so the tracked-
        // change torn-range repair can pair it with its inline partner. Without
        // this the half is invisible to the repair and a projection that removes
        // the paragraph holding the partner orphans the pair (§17.13.6).
        let range_marker = range_marker_meta_from_element(element);
        blocks.push(normal_tracked_block(BlockNode::from(OpaqueBlockNode {
            id: opaque_id,
            kind,
            opaque_ref: format!("body_item_{index}"),
            proof_ref,
            range_marker,
        })));
    }

    Ok(())
}

/// Classify a body-level element as one half of a paired range marker the
/// torn-range repair understands (bookmark / comment-range / permission), or
/// `None`. `customXml` and move ranges are intentionally excluded — they resolve
/// as a unit with their wrapper/move revision, not collapsed to a point.
fn range_marker_meta_from_element(el: &Element) -> Option<RangeMarkerMeta> {
    use crate::domain::{RangeMarkerFamily, RangeMarkerRole};
    let (family, role) = match local_element_name(el) {
        "bookmarkStart" => (RangeMarkerFamily::Bookmark, RangeMarkerRole::Start),
        "bookmarkEnd" => (RangeMarkerFamily::Bookmark, RangeMarkerRole::End),
        "commentRangeStart" => (RangeMarkerFamily::CommentRange, RangeMarkerRole::Start),
        "commentRangeEnd" => (RangeMarkerFamily::CommentRange, RangeMarkerRole::End),
        "permStart" => (RangeMarkerFamily::Permission, RangeMarkerRole::Start),
        "permEnd" => (RangeMarkerFamily::Permission, RangeMarkerRole::End),
        _ => return None,
    };
    Some(RangeMarkerMeta {
        family,
        id: attr_get(el, "id")?.clone(),
        role,
    })
}

/// Extract a `TrackingStatus` from a body-level tracked change element.
/// Handles w:ins, w:del, w:moveFrom (→ Deleted), and w:moveTo (→ Inserted).
fn extract_block_tracking_status(container: &Element) -> Result<TrackingStatus, RuntimeError> {
    let is_insertion = is_w_tag(container, "ins") || is_w_tag(container, "moveTo");
    let revision_id: u32 = attr_get(container, "id")
        .ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "body-level tracked change missing w:id".to_string(),
            details: ErrorDetails::default(),
        })?
        .parse()
        .map_err(|_| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "body-level tracked change w:id is not a valid u32".to_string(),
            details: ErrorDetails::default(),
        })?;
    let author = attr_get(container, "author").cloned();
    let date = attr_get(container, "date").cloned();
    let info = RevisionInfo {
        revision_id,
        author,
        date,
        apply_op_id: None,
    };
    Ok(if is_insertion {
        TrackingStatus::Inserted(info)
    } else {
        TrackingStatus::Deleted(info)
    })
}

fn element_contains_paragraph(element: &Element) -> bool {
    if is_w_tag(element, "p") {
        return true;
    }
    for child in &element.children {
        if let XMLNode::Element(el) = child
            && element_contains_paragraph(el)
        {
            return true;
        }
    }
    false
}
// ── Structure-level bookmark markers (§17.13.2 cross-structure annotations) ──
//
// `bookmarkStart`/`bookmarkEnd` may appear as direct children of `w:tbl`
// (between rows), `w:tr` (between/after cells — the §17.13.6.2 table-bookmark
// end shape), and `w:tc` (between blocks). The model has no slot between
// blocks/cells/rows, so these markers are re-anchored as zero-width
// Decoration inlines at the nearest paragraph boundary: PREPENDED to the
// first paragraph at-or-after the marker's position, or APPENDED to the last
// paragraph before it when nothing follows. The delimited CONTENT is
// unchanged (no content sits between the original position and the chosen
// boundary); only the boundary's relation to a paragraph mark can shift.
// Before this, the markers were silently dropped — tearing the pair and
// losing the bookmark (the defect the post-serialization bookmark guard now
// refuses).

/// Build a Decoration inline for a structure-level bookmark marker.
fn structural_bookmark_decoration(
    el: &Element,
    anchor_label: &str,
    ctx: &mut ParseContext,
) -> InlineNode {
    structural_range_decoration(el, anchor_label, DecorationType::Bookmark, ctx)
}

/// A structural (table/note/story-level) range marker preserved as a
/// paragraph-anchored decoration carrying the marker's verbatim bytes. Used for
/// cross-structure markers that live between rows / outside paragraphs —
/// bookmarks and customXml*Range markers — where dropping the marker would tear
/// its pair (the partner usually lives inside a cell or paragraph).
fn structural_range_decoration(
    el: &Element,
    anchor_label: &str,
    kind: DecorationType,
    ctx: &mut ParseContext,
) -> InlineNode {
    let local_index = *ctx.inline_counter;
    *ctx.inline_counter += 1;
    let id = NodeId::from(format!("{anchor_label}_deco_{local_index}"));
    let proof_ref = ProofRef {
        part: DocPart::DocumentXml,
        block_id: id.clone(),
        docx_anchor: format!("{anchor_label}:deco:{local_index}"),
    };
    InlineNode::from(DecorationNode {
        id: id.clone(),
        kind,
        opaque_ref: format!("{anchor_label}:deco:{local_index}"),
        proof_ref,
        // Paragraph-level range marker (bookmark/customXml range): no host run.
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(crate::word_xml::serialize_raw_fragment(el)),
        origin: None,
    })
}

/// True iff `el` is one of the four `customXml*Range{Start,End}` markers
/// (§17.13.5.4-.11), which may appear at table level between rows.
fn is_custom_xml_range_marker(el: &Element) -> bool {
    let local = local_element_name(el);
    local.starts_with("customXml") && (local.ends_with("RangeStart") || local.ends_with("RangeEnd"))
}

/// Prepend markers (in order) at the very front of a paragraph as a fresh
/// Normal segment — NOT inside an existing tracked segment, so a leading
/// Deleted segment cannot swallow the marker on accept.
fn prepend_markers_to_paragraph(para: &mut ParagraphNode, markers: Vec<InlineNode>) {
    para.segments.insert(
        0,
        TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: markers,
        },
    );
}

/// Append markers (in order) at the very end of a paragraph as a fresh
/// Normal segment.
fn append_markers_to_paragraph(para: &mut ParagraphNode, markers: Vec<InlineNode>) {
    para.segments.push(TrackedSegment {
        status: TrackingStatus::Normal,
        inlines: markers,
    });
}

/// Attach structure-level markers to a block list (table cells, story
/// roots). `pending` holds `(block_count_when_seen, marker)` in document
/// order.
fn attach_structural_markers_to_blocks(
    blocks: &mut [BlockNode],
    pending: Vec<(usize, InlineNode)>,
    diagnostics: &mut Vec<Diagnostic>,
    context: &str,
) {
    if pending.is_empty() {
        return;
    }
    let para_positions: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b, BlockNode::Paragraph(_)))
        .map(|(i, _)| i)
        .collect();
    // Bucket per target so multiple markers at one boundary keep their order.
    let mut prepend: std::collections::BTreeMap<usize, Vec<InlineNode>> =
        std::collections::BTreeMap::new();
    let mut append: std::collections::BTreeMap<usize, Vec<InlineNode>> =
        std::collections::BTreeMap::new();
    for (idx, marker) in pending {
        if let Some(&p) = para_positions.iter().find(|&&p| p >= idx) {
            prepend.entry(p).or_default().push(marker);
        } else if let Some(&p) = para_positions.iter().rev().find(|&&p| p < idx) {
            append.entry(p).or_default().push(marker);
        } else {
            // No paragraph anywhere (a vMerge-continue empty cell). Visible
            // drop — not silent — and vanishingly rare: CT_Tc (§17.4.73)
            // requires block content otherwise.
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                message: "dropped structure-level bookmark marker: no paragraph to anchor it"
                    .to_string(),
                context: Some(context.to_string()),
            });
        }
    }
    for (p, markers) in prepend {
        let BlockNode::Paragraph(para) = &mut blocks[p] else {
            unreachable!("para_positions only holds paragraph indices");
        };
        prepend_markers_to_paragraph(para, markers);
    }
    for (p, markers) in append {
        let BlockNode::Paragraph(para) = &mut blocks[p] else {
            unreachable!("para_positions only holds paragraph indices");
        };
        append_markers_to_paragraph(para, markers);
    }
}

/// True when the cell has a top-level paragraph to anchor a marker on.
fn cell_has_paragraph(cell: &TableCellNode) -> bool {
    cell.blocks
        .iter()
        .any(|b| matches!(b, BlockNode::Paragraph(_)))
}

/// Attach row-level markers (between/after cells) to the nearest cell
/// paragraph: prepend to the first paragraph of the first paragraph-bearing
/// cell at-or-after the position, else append to the last paragraph before it.
fn attach_row_level_markers(
    cells: &mut [TableCellNode],
    pending: Vec<(usize, InlineNode)>,
    diagnostics: &mut Vec<Diagnostic>,
    context: &str,
) {
    if pending.is_empty() {
        return;
    }
    let para_cells: Vec<usize> = cells
        .iter()
        .enumerate()
        .filter(|(_, c)| cell_has_paragraph(c))
        .map(|(i, _)| i)
        .collect();
    let mut prepend: std::collections::BTreeMap<usize, Vec<InlineNode>> =
        std::collections::BTreeMap::new();
    let mut append: std::collections::BTreeMap<usize, Vec<InlineNode>> =
        std::collections::BTreeMap::new();
    for (idx, marker) in pending {
        if let Some(&c) = para_cells.iter().find(|&&c| c >= idx) {
            prepend.entry(c).or_default().push(marker);
        } else if let Some(&c) = para_cells.iter().rev().find(|&&c| c < idx) {
            append.entry(c).or_default().push(marker);
        } else {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                message: "dropped row-level bookmark marker: no paragraph in any cell".to_string(),
                context: Some(context.to_string()),
            });
        }
    }
    for (c, markers) in prepend {
        let para = cells[c]
            .blocks
            .iter_mut()
            .find_map(|b| match b {
                BlockNode::Paragraph(p) => Some(p),
                _ => None,
            })
            .expect("para_cells only holds paragraph-bearing cells");
        prepend_markers_to_paragraph(para, markers);
    }
    for (c, markers) in append {
        let para = cells[c]
            .blocks
            .iter_mut()
            .rev()
            .find_map(|b| match b {
                BlockNode::Paragraph(p) => Some(p),
                _ => None,
            })
            .expect("para_cells only holds paragraph-bearing cells");
        append_markers_to_paragraph(para, markers);
    }
}

/// Attach table-level markers (between rows) analogously across rows.
fn attach_table_level_markers(
    rows: &mut [TableRowNode],
    pending: Vec<(usize, InlineNode)>,
    diagnostics: &mut Vec<Diagnostic>,
    context: &str,
) {
    if pending.is_empty() {
        return;
    }
    let para_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.cells.iter().any(cell_has_paragraph))
        .map(|(i, _)| i)
        .collect();
    let mut prepend: std::collections::BTreeMap<usize, Vec<InlineNode>> =
        std::collections::BTreeMap::new();
    let mut append: std::collections::BTreeMap<usize, Vec<InlineNode>> =
        std::collections::BTreeMap::new();
    for (idx, marker) in pending {
        if let Some(&r) = para_rows.iter().find(|&&r| r >= idx) {
            prepend.entry(r).or_default().push(marker);
        } else if let Some(&r) = para_rows.iter().rev().find(|&&r| r < idx) {
            append.entry(r).or_default().push(marker);
        } else {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                message: "dropped table-level bookmark marker: no paragraph in any row".to_string(),
                context: Some(context.to_string()),
            });
        }
    }
    for (r, markers) in prepend {
        let para = rows[r]
            .cells
            .iter_mut()
            .filter(|c| cell_has_paragraph(c))
            .flat_map(|c| c.blocks.iter_mut())
            .find_map(|b| match b {
                BlockNode::Paragraph(p) => Some(p),
                _ => None,
            })
            .expect("para_rows only holds paragraph-bearing rows");
        prepend_markers_to_paragraph(para, markers);
    }
    for (r, markers) in append {
        let para = rows[r]
            .cells
            .iter_mut()
            .rev()
            .filter(|c| cell_has_paragraph(c))
            .flat_map(|c| c.blocks.iter_mut().rev())
            .find_map(|b| match b {
                BlockNode::Paragraph(p) => Some(p),
                _ => None,
            })
            .expect("para_rows only holds paragraph-bearing rows");
        append_markers_to_paragraph(para, markers);
    }
}

/// Collect `w:tr` rows wrapped by a table-level `w:sdt` / `w:customXml` /
/// `w:smartTag`. Repeating-section content controls (CT_SdtRow, §17.5.2.30) and
/// customXml ranges may legally wrap whole rows; the wrapper is transparent for
/// row content. Rows are collected in document order; a `w:tr` is never
/// descended into. Nested wrappers recurse.
fn collect_wrapper_rows<'a>(wrapper: &'a Element, out: &mut Vec<&'a Element>) {
    // For an SDT the rows live under w:sdtContent; for customXml/smartTag the
    // children are direct (the wrapper is transparent).
    let children: &'a [XMLNode] = if is_w_tag(wrapper, "sdt") {
        match wrapper.children.iter().find_map(|c| match c {
            XMLNode::Element(e) if is_w_tag(e, "sdtContent") => Some(e),
            _ => None,
        }) {
            Some(content) => &content.children,
            None => return,
        }
    } else {
        &wrapper.children
    };
    for child in children {
        if let XMLNode::Element(e) = child {
            if is_w_tag(e, "tr") {
                out.push(e);
            } else if is_w_tag(e, "sdt") || is_w_tag(e, "customXml") || is_w_tag(e, "smartTag") {
                collect_wrapper_rows(e, out);
            }
        }
    }
}

/// Parse a w:tbl element into a TableNode.
fn table_from_element(
    element: &Element,
    table_counter: &mut u32,
    ctx: &mut ParseContext,
) -> Result<TableNode, RuntimeError> {
    let table_id = *table_counter;
    *table_counter += 1;

    let mut rows = Vec::new();
    let mut row_index = 0u32;
    let mut formatting = TableFormatting::default();
    let mut formatting_change: Option<TableFormattingChange> = None;
    let mut parsed_tbl_look = TblLook::default();
    let mut table_style_props: Option<&crate::styles::TableStyleProps> = None;
    // Table-level bookmark markers with the row count at their position.
    let mut pending_markers: Vec<(usize, InlineNode)> = Vec::new();

    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        // Parse table rows (w:tr)
        if is_w_tag(el, "tr") {
            let row = table_row_from_element(el, table_id, row_index, ctx)?;
            rows.push(row);
            row_index += 1;
            continue;
        }

        // Parse table properties (w:tblPr)
        if is_w_tag(el, "tblPr") {
            // Extract direct formatting from the tblPr element.
            let direct_borders = parse_border_set(el, "tblBorders")?;
            formatting.width = parse_table_measurement(el, "tblW")?;
            let direct_cell_margins = parse_cell_margins(el, "tblCellMar");

            // Direct table alignment from w:jc (§17.4.28).
            let direct_alignment = parse_table_alignment(el);
            // Direct table indent from w:tblInd (§17.4.51).
            let direct_indent = parse_table_indent(el);

            // Table layout from w:tblLayout w:type (§17.4.52).
            formatting.layout = parse_table_layout(el)?;
            // Cell spacing from w:tblCellSpacing w:w (§17.4.44).
            formatting.cell_spacing = parse_table_cell_spacing(el);
            // Floating table positioning from w:tblpPr (§17.4.57).
            formatting.positioning = parse_table_positioning(el)?;
            // Table overlap from w:tblOverlap w:val (§17.4.55).
            formatting.overlap = parse_table_overlap(el)?;
            // Band sizes from w:tblStyleRowBandSize/w:tblStyleColBandSize (§17.4.78/§17.4.79).
            formatting.row_band_size = parse_band_size(el, "tblStyleRowBandSize");
            formatting.col_band_size = parse_band_size(el, "tblStyleColBandSize");

            // Parse tblLook for conditional formatting flags. parse_tbl_look
            // returns the MS 0x04A0 default for an ABSENT element — record
            // authored presence so the serializer doesn't inject the default.
            parsed_tbl_look = parse_tbl_look(el);
            formatting.has_direct_tbl_look = el
                .children
                .iter()
                .any(|c| matches!(c, XMLNode::Element(e) if is_w_tag(e, "tblLook")));

            // Look up table style and merge: direct formatting wins over style-inherited.
            let style_id = el.children.iter().find_map(|c| {
                if let XMLNode::Element(child) = c
                    && is_w_tag(child, "tblStyle")
                {
                    return attr_get(child, "w:val").cloned();
                }
                None
            });
            let style_props = style_id
                .as_deref()
                .and_then(|id| ctx.style_defs.and_then(|defs| defs.table_style(id)));

            // Store style_id on formatting for roundtrip serialization.
            formatting.style_id = style_id.map(IStr::from);

            // Stash for conditional formatting post-processing.
            table_style_props = style_props;

            // The VALUE fields hold the resolved (direct-or-style) formatting
            // for projections; provenance flags record whether the table's own
            // tblPr authored each slot, so style-inherited values are never
            // materialized as direct markup on save.
            formatting.has_direct_borders = direct_borders.is_some();
            formatting.has_direct_cell_margins = direct_cell_margins.is_some();
            formatting.has_direct_alignment = direct_alignment.is_some();
            formatting.has_direct_indent = direct_indent.is_some();
            formatting.borders =
                direct_borders.or_else(|| style_props.and_then(|sp| sp.borders.clone()));
            formatting.default_cell_margins = direct_cell_margins
                .or_else(|| style_props.and_then(|sp| sp.default_cell_margins.clone()));
            formatting.alignment =
                direct_alignment.or_else(|| style_props.and_then(|sp| sp.alignment.clone()));
            formatting.indent = direct_indent.or_else(|| style_props.and_then(|sp| sp.indent));
            // Table-level shading (w:shd, §17.4.32) — distinct from cell shd.
            formatting.shading = parse_shading(el)?;
            // RTL visual column order (w:bidiVisual, §17.4.1) — presence = true.
            formatting.bidi_visual = el
                .children
                .iter()
                .any(|c| matches!(c, XMLNode::Element(e) if is_w_tag(e, "bidiVisual")));
            // Accessibility caption/description (w:tblCaption §17.4.42 /
            // w:tblDescription §17.4.46) — w:val string.
            formatting.caption = find_w_child_val(el, "tblCaption");
            formatting.description = find_w_child_val(el, "tblDescription");
            // RFC-0003 "never silently drop": capture any tblPr child the typed
            // fields don't model (vendor extensions, future OOXML) verbatim.
            formatting.preserved =
                capture_unmodeled_children(el, crate::docx_validate_ordering::TBLPR_ORDER);
            formatting_change = parse_tbl_pr_change(el)?;
            continue;
        }

        // Parse table grid (w:tblGrid) — column widths
        if is_w_tag(el, "tblGrid") {
            formatting.grid_cols = parse_grid_cols(el)?;
            continue;
        }

        // Skip tblPrChange at table level (malformed but tolerated)
        if is_w_tag(el, "tblPrChange") {
            continue;
        }

        // Bookmark markers between rows (§17.13.2 cross-structure
        // annotations): preserve as paragraph-anchored decorations — dropping
        // them tears the pair (the other half usually lives inside a cell).
        if is_w_tag(el, "bookmarkStart") || is_w_tag(el, "bookmarkEnd") {
            pending_markers.push((
                rows.len(),
                structural_bookmark_decoration(el, &format!("tbl_{table_id}"), ctx),
            ));
            continue;
        }

        // customXml*Range markers between rows (§17.13.5.4-.11 cross-structure
        // revision ranges): preserve as paragraph-anchored decorations —
        // dropping them tears the pair (the other half usually lives inside a
        // cell, and a torn pair is non-conformant per I-ANN-009). All four
        // families (Ins/Del/MoveFrom/MoveTo), not just Ins.
        if is_custom_xml_range_marker(el) {
            pending_markers.push((
                rows.len(),
                structural_range_decoration(
                    el,
                    &format!("tbl_{table_id}"),
                    DecorationType::CustomXmlRange,
                    ctx,
                ),
            ));
            continue;
        }

        // w:sdt / w:customXml / w:smartTag wrapping whole rows (repeating-section
        // content controls, §17.5.2.30 CT_SdtRow; customXml is a transparent
        // revision/binding wrapper). The loop above only matches a bare w:tr, so
        // without this the wrapped rows fall through to the unknown-element
        // diagnostic and are LOST — and because tables are fully rebuilt on
        // materialization, the row content vanishes from the output (P0 #12).
        // Recover the rows so their content survives. The wrapper element itself
        // is flattened (a Warning records it); full repeating-section wrapper
        // preservation is a separate model feature.
        if is_w_tag(el, "sdt") || is_w_tag(el, "customXml") || is_w_tag(el, "smartTag") {
            let mut wrapped_rows = Vec::new();
            collect_wrapper_rows(el, &mut wrapped_rows);
            if !wrapped_rows.is_empty() {
                ctx.diagnostics.push(Diagnostic {
                    level: DiagnosticLevel::Warning,
                    message: format!(
                        "table-level <{}> wrapping {} row(s) was flattened; the row content is \
                         preserved but the wrapper is not",
                        el.name,
                        wrapped_rows.len()
                    ),
                    context: Some(format!("tbl_{table_id}")),
                });
                for tr in wrapped_rows {
                    let row = table_row_from_element(tr, table_id, row_index, ctx)?;
                    rows.push(row);
                    row_index += 1;
                }
                continue;
            }
        }

        // Log diagnostic for unknown table-level elements (graceful degradation)
        ctx.diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Info,
            message: format!("unknown table child element: {}", el.name),
            context: Some(format!("tbl_{table_id}")),
        });
    }

    attach_table_level_markers(
        &mut rows,
        pending_markers,
        ctx.diagnostics,
        &format!("tbl_{table_id}"),
    );

    // Zero rows is a VALID state: the transitional schema's CT_Tbl puts the
    // row group (EG_ContentRowContent) at minOccurs="0" — a table carrying
    // only tblPr + tblGrid is schema-valid, wild Word-authored documents
    // contain such tables, and Word opens them without repair. The canonical
    // model represents this as a TableNode with empty `rows` (grid and
    // formatting preserved); canonicalize_table, the serializer, and the
    // structural table verbs all handle the empty-rows case explicitly.

    // Post-process: apply conditional formatting from table style to cells.
    if let Some(style_props) = table_style_props {
        if !style_props.conditional.is_empty() {
            apply_conditional_formatting(
                &mut rows,
                &style_props.conditional,
                &parsed_tbl_look,
                &style_props.default_cell_shading,
                style_props.row_band_size,
                style_props.col_band_size,
                style_props.base_bold,
                style_props.base_color.as_ref(),
                style_props.base_font_family.as_ref(),
            );
        }
        // Always apply default cell shading (root-level tcPr/shd) as a fallback.
        // apply_default_cell_shading only sets shading on cells that are still None,
        // so higher-precedence conditionals win.
        if style_props.default_cell_shading.is_some() {
            apply_default_cell_shading(&mut rows, &style_props.default_cell_shading);
        }
    }

    // Post-process: MS-DOCX §2.3.1 — when overrideTableStyleFontSizeAndJustification is
    // absent or false, the default paragraph style's font size and justification do NOT
    // override the table style's base pPr/rPr values. Apply the table style's base
    // alignment and font size to cell paragraphs using the default paragraph style.
    if let Some(style_props) = table_style_props {
        let override_compat = ctx
            .compat_settings
            .override_table_style_font_size_and_justification
            .unwrap_or(false);
        if !override_compat {
            // ECMA-376 §17.7.4.17: the default paragraph style is "Normal" unless
            // explicitly overridden via w:default="1" on another paragraph style.
            let default_para_id = ctx
                .style_defs
                .and_then(|sd| sd.default_para_style_id())
                .unwrap_or("Normal");
            apply_table_style_base_props(
                &mut rows,
                style_props.base_para_alignment.as_ref(),
                style_props.base_font_size,
                default_para_id,
            );
        }
    }

    // Post-process: apply base run props when no conditionals are present.
    // When conditionals exist, base run props are applied inside apply_conditional_formatting
    // as lowest-precedence fallbacks. When there are no conditionals, apply them directly.
    if let Some(style_props) = table_style_props
        && style_props.conditional.is_empty()
    {
        apply_table_style_base_run_props(
            &mut rows,
            style_props.base_bold,
            style_props.base_color.as_ref(),
            style_props.base_font_family.as_ref(),
        );
    }

    // Store tbl_look on formatting for roundtrip serialization.
    formatting.tbl_look = Some(parsed_tbl_look);

    // Post-process: clamp gridSpan to not exceed tblGrid column count (§17.4.17).
    let grid_col_count = formatting.grid_cols.len() as u32;
    if grid_col_count > 0 {
        for row in &mut rows {
            for cell in &mut row.cells {
                if cell.grid_span > grid_col_count {
                    cell.grid_span = grid_col_count;
                }
            }
        }
    }

    // Post-process: validate vMerge grid alignment (§17.4.84).
    normalize_vmerge_grid_alignment(&mut rows);

    // Post-process: resolve border conflicts between table and cell borders
    // (MS-OI29500 §17.4.66(a)). Higher weight wins.
    resolve_table_cell_border_conflicts(&mut rows, &formatting.borders);

    // Post-process: resolve border conflicts between adjacent cells at shared
    // edges (ISO 29500-1 §17.4.66 rule 3). The heavier border wins.
    resolve_adjacent_cell_border_conflicts(&mut rows);

    let structure_hash = compute_table_structure_hash(&rows);

    Ok(TableNode {
        id: NodeId::from(format!("tbl_{table_id}")),
        rows,
        structure_hash,
        formatting,
        formatting_change,
    })
}

/// Parse a w:tr element into a TableRowNode.
fn table_row_from_element(
    element: &Element,
    table_id: u32,
    row_index: u32,
    ctx: &mut ParseContext,
) -> Result<TableRowNode, RuntimeError> {
    let mut cells = Vec::new();
    let mut cell_index = 0u32;
    let mut grid_before: u32 = 0;
    let mut grid_after: u32 = 0;
    let mut tracking_status: Option<TrackingStatus> = None;
    let mut row_ins: Option<RevisionInfo> = None;
    let mut row_del: Option<RevisionInfo> = None;
    // Row-level bookmark markers with the cell count at their position.
    let mut pending_markers: Vec<(usize, InlineNode)> = Vec::new();
    let mut is_header = false;
    let mut height: Option<u32> = None;
    let mut height_rule: Option<HeightRule> = None;
    let mut formatting_change: Option<RowFormattingChange> = None;
    let mut cant_split = false;
    let mut jc: Option<Alignment> = None;
    let mut w_before: Option<TableMeasurement> = None;
    let mut w_after: Option<TableMeasurement> = None;
    let mut cnf_style: Option<crate::domain::CnfStyle> = None;
    let mut tbl_pr_ex: Option<TableFormatting> = None;
    let mut cell_spacing: Option<i64> = None;
    let mut preserved: Vec<crate::domain::PreservedProp> = Vec::new();

    // MS-DOCX §2.2.4: w14:paraId and w14:textId apply to w:tr elements.
    let para_id = attr_get(element, "w14:paraId").map(|s| s.to_string());
    let text_id = attr_get(element, "w14:textId").map(|s| s.to_string());

    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        // Parse table cells (w:tc)
        if is_w_tag(el, "tc") {
            let cell = table_cell_from_element(el, table_id, row_index, cell_index, ctx)?;
            cells.push(cell);
            cell_index += 1;
            continue;
        }

        // Parse row properties (w:trPr) for gridBefore/gridAfter and tracking status
        if is_w_tag(el, "trPr") {
            for prop in &el.children {
                if let XMLNode::Element(prop_el) = prop {
                    if is_w_tag(prop_el, "gridBefore")
                        && let Some(val) = attr_get(prop_el, "w:val")
                    {
                        // ST_DecimalNumber is signed (xsd:integer). Word ignores
                        // 0/negative gridBefore and opens the file (MS-OI29500
                        // §2.1.129), so clamp negatives to 0 rather than refusing
                        // a document Word accepts (confirmed against real Word).
                        grid_before = val
                            .parse::<i64>()
                            .map_err(|_| {
                                invalid_docx(&format!("gridBefore: invalid value {val:?}"))
                            })?
                            .max(0) as u32;
                    }
                    if is_w_tag(prop_el, "gridAfter")
                        && let Some(val) = attr_get(prop_el, "w:val")
                    {
                        // Word ignores 0/negative gridAfter (MS-OI29500 §2.1.128);
                        // clamp instead of erroring (confirmed against real Word).
                        grid_after = val
                            .parse::<i64>()
                            .map_err(|_| {
                                invalid_docx(&format!("gridAfter: invalid value {val:?}"))
                            })?
                            .max(0) as u32;
                    }
                    // Parse tblHeader (header row repeat)
                    if is_w_tag(prop_el, "tblHeader") {
                        is_header = true;
                    }
                    // Parse cantSplit (row may not be split across pages, §17.4.6).
                    if is_w_tag(prop_el, "cantSplit") {
                        cant_split = !matches!(
                            attr_get(prop_el, "w:val").map(|s| s.as_str()),
                            Some("0") | Some("false") | Some("off")
                        );
                    }
                    // Parse cnfStyle (row conditional formatting, §17.4.7).
                    if is_w_tag(prop_el, "cnfStyle") {
                        cnf_style = parse_cnf_style(prop_el);
                    }
                    // Parse trHeight (row height)
                    if is_w_tag(prop_el, "trHeight") {
                        height = attr_get(prop_el, "w:val")
                            .map(|v| parse_twips_measure(v, "trHeight element"))
                            .transpose()?;
                        height_rule = attr_get(prop_el, "w:hRule")
                            .map(|s| HeightRule::from_xml_str(s))
                            .transpose()
                            .map_err(|e| invalid_docx(&format!("trHeight: {e}")))?;
                    }
                    // Detect row-level tracked changes (w:ins, w:del).
                    // A row can carry BOTH markers — inserted by one pending
                    // revision, deleted by another (the row-level stacked
                    // state; real EBA/EMA corpus documents have this). Word's
                    // semantics mirror the inline four origin rules (verified
                    // against real Word): both full resolutions drop the row; only the
                    // mixed accept-insert + reject-delete resolution keeps it.
                    if is_w_tag(prop_el, "ins") {
                        let revision_id = parse_revision_id(prop_el, "w:ins")?;
                        row_ins = Some(RevisionInfo {
                            revision_id,
                            author: attr_get(prop_el, "w:author").cloned(),
                            date: attr_get(prop_el, "w:date").cloned(),
                            apply_op_id: None,
                        });
                    }
                    if is_w_tag(prop_el, "del") {
                        let revision_id = parse_revision_id(prop_el, "w:del")?;
                        row_del = Some(RevisionInfo {
                            revision_id,
                            author: attr_get(prop_el, "w:author").cloned(),
                            date: attr_get(prop_el, "w:date").cloned(),
                            apply_op_id: None,
                        });
                    }
                }
            }
            tracking_status = match (row_ins.take(), row_del.take()) {
                (Some(inserted), Some(deleted)) => Some(TrackingStatus::InsertedThenDeleted(
                    Box::new(crate::domain::StackedRevision { inserted, deleted }),
                )),
                (Some(rev), None) => Some(TrackingStatus::Inserted(rev)),
                (None, Some(rev)) => Some(TrackingStatus::Deleted(rev)),
                (None, None) => tracking_status,
            };
            formatting_change = parse_tr_pr_change(el)?;
            // Preferred widths of the gridBefore/gridAfter empty spans
            // (w:wBefore §17.4.86 / w:wAfter §17.4.85, CT_TblWidth).
            w_before = parse_table_measurement(el, "wBefore")?;
            w_after = parse_table_measurement(el, "wAfter")?;
            // Row-level table justification (w:jc in trPr, §17.4.28).
            jc = parse_table_alignment(el);
            // Row-level cell spacing (w:tblCellSpacing in trPr, §17.4.44).
            // Reuses the table-level parser (scans for the child); without this
            // a row whose only trPr content is tblCellSpacing loses its trPr.
            cell_spacing = parse_table_cell_spacing(el);
            // RFC-0003 "never silently drop": capture any trPr child the typed
            // fields don't consume — w:divId, w:hidden, vendor extensions.
            preserved = capture_unmodeled_children(el, TRPR_CONSUMED);
            continue;
        }

        // Row-level table property exceptions (w:tblPrEx, §17.4.61) — a direct
        // child of w:tr, NOT inside trPr. Per-row override of table properties.
        if is_w_tag(el, "tblPrEx") {
            tbl_pr_ex = Some(parse_tbl_pr_ex(el)?);
            continue;
        }

        // Handle SDT-wrapped cells - traverse into sdtContent to find cells,
        // preserving the SDT wrapper properties for roundtripping.
        // SDTs can be nested (CT_SdtCell content recursively references
        // EG_ContentCellContent), so we recursively descend through
        // sdt > sdtContent layers until we find tc elements.
        if is_w_tag(el, "sdt") {
            let sdt_wrapper = extract_sdt_wrapper(el)?;
            fn collect_cells_from_sdt(
                sdt_el: &Element,
                table_id: u32,
                row_index: u32,
                cell_index: &mut u32,
                sdt_wrapper: &SdtWrapper,
                cells: &mut Vec<TableCellNode>,
                ctx: &mut ParseContext,
            ) -> Result<(), RuntimeError> {
                for sdt_child in &sdt_el.children {
                    if let XMLNode::Element(content_el) = sdt_child
                        && is_w_tag(content_el, "sdtContent")
                    {
                        for content_child in &content_el.children {
                            if let XMLNode::Element(inner_el) = content_child {
                                if is_w_tag(inner_el, "tc") {
                                    let mut cell = table_cell_from_element(
                                        inner_el,
                                        table_id,
                                        row_index,
                                        *cell_index,
                                        ctx,
                                    )?;
                                    cell.row_sdt_wrapper = Some(sdt_wrapper.clone());
                                    cells.push(cell);
                                    *cell_index += 1;
                                } else if is_w_tag(inner_el, "sdt") {
                                    // Recurse into nested SDT
                                    collect_cells_from_sdt(
                                        inner_el,
                                        table_id,
                                        row_index,
                                        cell_index,
                                        sdt_wrapper,
                                        cells,
                                        ctx,
                                    )?;
                                }
                            }
                        }
                    }
                }
                Ok(())
            }
            collect_cells_from_sdt(
                el,
                table_id,
                row_index,
                &mut cell_index,
                &sdt_wrapper,
                &mut cells,
                ctx,
            )?;
            continue;
        }

        // Bookmark markers between/after cells: the §17.13.6.2 table-bookmark
        // end shape ("bookmarkEnd ... at the end of that table row") and the
        // wild `_GoBack` placements. Preserve as paragraph-anchored
        // decorations — dropping them tears the pair.
        if is_w_tag(el, "bookmarkStart") || is_w_tag(el, "bookmarkEnd") {
            pending_markers.push((
                cells.len(),
                structural_bookmark_decoration(el, &format!("tbl_{table_id}_r{row_index}"), ctx),
            ));
            continue;
        }

        // customXml*Range markers between/after cells (§17.13.5.4-.11):
        // preserve as paragraph-anchored decorations like bookmarks — dropping
        // them tears the pair (I-ANN-009). All four families, not just Ins.
        if is_custom_xml_range_marker(el) {
            pending_markers.push((
                cells.len(),
                structural_range_decoration(
                    el,
                    &format!("tbl_{table_id}_r{row_index}"),
                    DecorationType::CustomXmlRange,
                    ctx,
                ),
            ));
            continue;
        }

        // Skip tracked change wrappers - traverse into them (graceful degradation)
        if is_w_tag(el, "ins")
            || is_w_tag(el, "del")
            || is_w_tag(el, "moveFrom")
            || is_w_tag(el, "moveTo")
        {
            // Traverse into the wrapper to find actual content
            for inner_child in &el.children {
                if let XMLNode::Element(inner_el) = inner_child
                    && is_w_tag(inner_el, "tc")
                {
                    let cell =
                        table_cell_from_element(inner_el, table_id, row_index, cell_index, ctx)?;
                    cells.push(cell);
                    cell_index += 1;
                }
            }
            continue;
        }

        // Log diagnostic for unknown row-level elements
        ctx.diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Info,
            message: format!("unknown table row child element: {}", el.name),
            context: Some(format!("tbl_{table_id}_r{row_index}")),
        });
    }

    // OOXML §17.4.72 CT_Row requires at least one cell (tc+).
    if cells.is_empty() {
        return Err(invalid_docx(&format!(
            "table row tbl_{table_id}_r{row_index} must contain at least one cell (OOXML §17.4.72 CT_Row requires tc+)"
        )));
    }

    attach_row_level_markers(
        &mut cells,
        pending_markers,
        ctx.diagnostics,
        &format!("tbl_{table_id}_r{row_index}"),
    );

    // MS-OI29500 §17.4.80(a): When hRule is omitted but height is present,
    // Word defaults to "atLeast" (not "auto" as the base spec says).
    let effective_height_rule = if height.is_some() && height_rule.is_none() {
        Some(HeightRule::AtLeast)
    } else {
        height_rule
    };

    Ok(TableRowNode {
        id: NodeId::from(format!("tbl_{table_id}_r{row_index}")),
        cells,
        grid_before,
        grid_after,
        tracking_status,
        is_header,
        height,
        height_rule: effective_height_rule,
        formatting_change,
        para_id,
        text_id,
        cant_split,
        jc,
        w_before,
        w_after,
        cnf_style,
        tbl_pr_ex,
        cell_spacing,
        preserved,
    })
}

/// Parse a w:tc element into a TableCellNode.
fn table_cell_from_element(
    element: &Element,
    table_id: u32,
    row_index: u32,
    cell_index: u32,
    ctx: &mut ParseContext,
) -> Result<TableCellNode, RuntimeError> {
    let mut blocks = Vec::new();
    let mut opaque_counter = 1u32;
    let mut nested_table_counter = 1u32;
    // Cell-level bookmark markers with the block count at their position.
    let mut pending_markers: Vec<(usize, InlineNode)> = Vec::new();
    let mut grid_span: u32 = 1;
    let mut v_merge = VerticalMerge::None;
    let mut formatting = CellFormatting::default();
    let mut formatting_change: Option<CellFormattingChange> = None;
    let mut tracking_status: Option<TrackingStatus> = None;
    let mut cell_ins: Option<RevisionInfo> = None;
    let mut cell_del: Option<RevisionInfo> = None;
    let mut content_sdt_wraps: Vec<CellSdtWrap> = Vec::new();
    let mut cnf_style: Option<crate::domain::CnfStyle> = None;
    let mut hide_mark = false;
    let mut preserved: Vec<crate::domain::PreservedProp> = Vec::new();
    let mut cell_ctx = ParseContext {
        diagnostics: ctx.diagnostics,
        opaque_counter: &mut opaque_counter,
        inline_counter: ctx.inline_counter,
        block_id_counter: ctx.block_id_counter,
        numbering_defs: ctx.numbering_defs,
        numbering_state: ctx.numbering_state,
        style_defs: ctx.style_defs,
        default_tab_stop: ctx.default_tab_stop,
        compat_settings: ctx.compat_settings,
        rel_lookup: ctx.rel_lookup,
        active_move_name: None,
        active_move_status: None,
    };

    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        // Parse cell properties (w:tcPr) for gridSpan, vMerge, and formatting
        if is_w_tag(el, "tcPr") {
            for prop in &el.children {
                if let XMLNode::Element(prop_el) = prop {
                    // Parse gridSpan (horizontal merge)
                    if is_w_tag(prop_el, "gridSpan")
                        && let Some(val) = attr_get(prop_el, "w:val")
                    {
                        grid_span = val.parse().map_err(|_| RuntimeError {
                            code: ErrorCode::InvalidDocx,
                            message: format!(
                                "invalid gridSpan value '{val}' in cell tbl_{table_id}_r{row_index}_c{cell_index}"
                            ),
                            details: ErrorDetails::default(),
                        })?;
                    }
                    // Parse vMerge (vertical merge)
                    if is_w_tag(prop_el, "vMerge") {
                        v_merge = match attr_get(prop_el, "w:val").map(|s| s.as_str()) {
                            Some("restart") => VerticalMerge::Restart,
                            Some("continue") | None => VerticalMerge::Continue,
                            _ => VerticalMerge::None,
                        };
                    }
                    // Parse cnfStyle (cell conditional formatting, §17.4.7).
                    if is_w_tag(prop_el, "cnfStyle") {
                        cnf_style = parse_cnf_style(prop_el);
                    }
                    // Parse hideMark (hidden end-of-cell mark, §17.4.10).
                    if is_w_tag(prop_el, "hideMark") {
                        hide_mark = !matches!(
                            attr_get(prop_el, "w:val").map(|s| s.as_str()),
                            Some("0") | Some("false") | Some("off")
                        );
                    }
                    // Parse vAlign (vertical alignment)
                    if is_w_tag(prop_el, "vAlign")
                        && let Some(val) = attr_get(prop_el, "w:val")
                    {
                        formatting.v_align = match val.as_str() {
                            "top" => Some(VerticalAlignment::Top),
                            "center" => Some(VerticalAlignment::Center),
                            "bottom" => Some(VerticalAlignment::Bottom),
                            _ => None,
                        };
                    }
                    // Parse noWrap (no text wrapping, §17.4.30)
                    if is_w_tag(prop_el, "noWrap") {
                        formatting.no_wrap = Some(!matches!(
                            attr_get(prop_el, "w:val").map(|s| s.as_str()),
                            Some("0") | Some("false") | Some("off")
                        ));
                    }
                    // Parse textDirection (§17.4.72)
                    if is_w_tag(prop_el, "textDirection")
                        && let Some(val) = attr_get(prop_el, "w:val")
                        && let Ok(td) = TextDirection::from_xml_str(val)
                    {
                        formatting.text_direction = Some(td);
                    }
                    // Parse tcFitText (fit text to cell width, §17.4.63)
                    if is_w_tag(prop_el, "tcFitText") {
                        formatting.tc_fit_text = Some(!matches!(
                            attr_get(prop_el, "w:val").map(|s| s.as_str()),
                            Some("0") | Some("false") | Some("off")
                        ));
                    }
                    // Detect cell-level tracked changes (w:cellIns, w:cellDel).
                    // Both markers together = the cell-level stacked state
                    // (inserted by one pending revision, deleted by another),
                    // the same shape rows and paragraph marks carry.
                    if is_w_tag(prop_el, "cellIns") {
                        let revision_id = parse_revision_id(prop_el, "w:cellIns")?;
                        cell_ins = Some(RevisionInfo {
                            revision_id,
                            author: attr_get(prop_el, "w:author").cloned(),
                            date: attr_get(prop_el, "w:date").cloned(),
                            apply_op_id: None,
                        });
                    }
                    if is_w_tag(prop_el, "cellDel") {
                        let revision_id = parse_revision_id(prop_el, "w:cellDel")?;
                        cell_del = Some(RevisionInfo {
                            revision_id,
                            author: attr_get(prop_el, "w:author").cloned(),
                            date: attr_get(prop_el, "w:date").cloned(),
                            apply_op_id: None,
                        });
                    }
                }
            }
            // Parse cell-level borders, shading, width, margins from tcPr.
            // Provenance: post-processing (conditional banding, default cell
            // shading, border-conflict resolution) mutates the VALUES for
            // projections — the flags keep the serializer emitting only what
            // this tcPr authored.
            formatting.borders = parse_border_set(el, "tcBorders")?;
            // Snapshot the authored tcBorders before table/adjacent-cell border
            // resolution (below) overwrites `borders` with the effective set.
            // The serializer emits this authored snapshot so an edge the author
            // omitted stays absent on round-trip (§17.4.39).
            formatting.authored_borders = formatting.borders.clone();
            formatting.shading = parse_shading(el)?;
            formatting.has_direct_borders = formatting.borders.is_some();
            formatting.has_direct_shading = formatting.shading.is_some();
            formatting.width = parse_table_measurement(el, "tcW")?;
            formatting.margins = parse_cell_margins(el, "tcMar");
            tracking_status = match (cell_ins.take(), cell_del.take()) {
                (Some(inserted), Some(deleted)) => Some(TrackingStatus::InsertedThenDeleted(
                    Box::new(crate::domain::StackedRevision { inserted, deleted }),
                )),
                (Some(rev), None) => Some(TrackingStatus::Inserted(rev)),
                (None, Some(rev)) => Some(TrackingStatus::Deleted(rev)),
                (None, None) => tracking_status,
            };
            formatting_change = parse_tc_pr_change(el)?;
            // RFC-0003 "never silently drop": capture any tcPr child the typed
            // fields don't consume — legacy w:hMerge, vendor extensions.
            preserved = capture_unmodeled_children(el, TCPR_CONSUMED);
            continue;
        }

        // Handle a block-level w:sdt inside the cell: descend into its
        // w:sdtContent AND record the exact range it wraps. A cell can hold
        // several SDTs interleaved with unwrapped sibling blocks (a checkbox
        // control's single glyph paragraph followed by a sibling label
        // paragraph, say), so we record WHERE this wrap starts and HOW MANY
        // blocks its sdtContent contributed — not a single whole-cell wrapper.
        // Without the span, the following sibling gets re-nested inside the
        // control on export and Word repairs the file (§17.5.2: a content
        // control binds a fixed run count).
        if is_w_tag(el, "sdt") {
            let wrapper = extract_sdt_wrapper(el)?;
            let start = blocks.len();
            // Find the sdtContent and traverse into it.
            for sdt_child in &el.children {
                if let XMLNode::Element(sdt_el) = sdt_child
                    && is_w_tag(sdt_el, "sdtContent")
                {
                    for content_child in &sdt_el.children {
                        if let XMLNode::Element(content_el) = content_child {
                            append_blocks_from_element(
                                content_el,
                                None,
                                &mut blocks,
                                &mut nested_table_counter,
                                &mut cell_ctx,
                            )?;
                        }
                    }
                }
            }
            let span = blocks.len() - start;
            if span >= 1 {
                content_sdt_wraps.push(CellSdtWrap {
                    start,
                    span,
                    wrapper,
                });
            } else {
                // An SDT whose sdtContent held no block content: nothing to
                // wrap. Drop the empty wrapper with a diagnostic rather than
                // fabricate a zero-span range (visible, not silent).
                cell_ctx.diagnostics.push(Diagnostic {
                    level: DiagnosticLevel::Warning,
                    message: "cell-content <w:sdt> with no block content dropped".to_string(),
                    context: Some(format!("tbl_{table_id}_r{row_index}_c{cell_index}")),
                });
            }
            continue;
        }

        // Bookmark markers between the cell's blocks (§17.13.2): preserve as
        // paragraph-anchored decorations. Before this they fell into
        // `append_blocks_from_element` with no body anchor and were silently
        // dropped, tearing the pair.
        if is_w_tag(el, "bookmarkStart") || is_w_tag(el, "bookmarkEnd") {
            pending_markers.push((
                blocks.len(),
                structural_bookmark_decoration(
                    el,
                    &format!("tbl_{table_id}_r{row_index}_c{cell_index}"),
                    &mut cell_ctx,
                ),
            ));
            continue;
        }

        // Recurse into cell content using existing block parsing
        append_blocks_from_element(
            el,
            None,
            &mut blocks,
            &mut nested_table_counter,
            &mut cell_ctx,
        )?;
    }

    // Word ignores several paragraph properties inside table cells — but that
    // is a RENDER-time consumption rule (MS-OI29500 §2.1.46 keepLines,
    // §2.1.66 widowControl), not a save rewrite: Word preserves the authored
    // markup verbatim. keepLines / widowControl therefore stay in the model
    // (with their has_direct_* provenance) and round-trip; renderers apply the
    // ignore-rule at consumption. Only sectPr is still cleared: a
    // cell-paragraph sectPr is structurally unemittable here (§17.6.18c) and
    // carrying it would fabricate a section boundary.
    let mut blocks: Vec<BlockNode> = blocks
        .into_iter()
        .map(|tb| match tb.block {
            BlockNode::Paragraph(mut p) => {
                // MS-OI29500 §17.6.18c: Word ignores sectPr in table cell paragraphs.
                p.section_properties = None;
                p.section_property_change = None;
                BlockNode::Paragraph(p)
            }
            other => other,
        })
        .collect();

    attach_structural_markers_to_blocks(
        &mut blocks,
        pending_markers,
        ctx.diagnostics,
        &format!("tbl_{table_id}_r{row_index}_c{cell_index}"),
    );

    // OOXML §17.4.73 CT_Tc requires at least one block element (p|tbl)+.
    // Cells that are continuation cells in a vertical merge (vMerge=continue) may be empty.
    if blocks.is_empty() && v_merge != VerticalMerge::Continue {
        return Err(invalid_docx(&format!(
            "table cell tbl_{table_id}_r{row_index}_c{cell_index} must contain at least one block element (OOXML §17.4.73 CT_Tc requires (p|tbl)+)"
        )));
    }

    Ok(TableCellNode {
        id: NodeId::from(format!("tbl_{table_id}_r{row_index}_c{cell_index}")),
        blocks,
        grid_span,
        v_merge,
        formatting,
        formatting_change,
        tracking_status,
        row_sdt_wrapper: None,
        content_sdt_wraps,
        cnf_style,
        hide_mark,
        preserved,
    })
}
// =============================================================================
// SDT (Content Control) Parsing Helpers
// =============================================================================

/// Extract SDT wrapper properties from a `w:sdt` element.
/// Serializes `w:sdtPr` and `w:sdtEndPr` children to raw XML bytes
/// so the wrapper can be reconstructed during serialization.
fn extract_sdt_wrapper(sdt_element: &Element) -> Result<SdtWrapper, RuntimeError> {
    // Serialize each property element as a SELF-CONTAINED fragment: the raw
    // bytes must declare every namespace prefix they use, so they re-parse
    // standalone in `build_sdt_wrapper`. This uses the same `serialize_raw_fragment`
    // / `parse_raw_fragment` pair the rest of the engine uses for opaque
    // fragments. (A plain `el.write()` would only declare prefixes that happen to
    // sit in `el.namespaces`; inner elements carry no inherited scope, so the
    // bytes would reference an unbound `w:` prefix.)
    let mut sdt_pr_xml = Vec::new();
    let mut sdt_end_pr_xml = None;
    for child in &sdt_element.children {
        if let XMLNode::Element(el) = child {
            if is_w_tag(el, "sdtPr") {
                sdt_pr_xml = crate::word_xml::serialize_raw_fragment(el);
            } else if is_w_tag(el, "sdtEndPr") {
                sdt_end_pr_xml = Some(crate::word_xml::serialize_raw_fragment(el));
            }
        }
    }
    Ok(SdtWrapper {
        sdt_pr_xml,
        sdt_end_pr_xml,
    })
}
// =============================================================================
// Table Formatting Parsing Helpers
// =============================================================================

/// Parse table alignment from w:jc child of a w:tblPr element (§17.4.28).
fn parse_table_alignment(tbl_pr: &Element) -> Option<Alignment> {
    for child in &tbl_pr.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "jc")
        {
            return attr_get(el, "w:val").and_then(|v| match v.as_str() {
                "left" | "start" => Some(Alignment::Left),
                "center" => Some(Alignment::Center),
                "right" | "end" => Some(Alignment::Right),
                "distribute" => Some(Alignment::Distribute),
                "highKashida" => Some(Alignment::HighKashida),
                "lowKashida" => Some(Alignment::LowKashida),
                "mediumKashida" => Some(Alignment::MediumKashida),
                "numTab" => Some(Alignment::NumTab),
                "thaiDistribute" => Some(Alignment::ThaiDistribute),
                _ => None,
            });
        }
    }
    None
}

/// Parse table indent from w:tblInd child of a w:tblPr element (§17.4.51).
///
/// `w:w` is ST_MeasurementOrPercent via CT_TblWidth: plain numbers and
/// universal measures ("1in") both resolve to twips, the unit the indent
/// model stores. OBSERVABLE BOUNDARY for the remaining forms: a percent
/// indent has no twips meaning (Word ignores it for tblInd) and an
/// out-of-range/invalid value cannot be stored — both are dropped (indent
/// absent) with a warning rather than refusing a document Word opens fine.
fn parse_table_indent(tbl_pr: &Element) -> Option<i32> {
    for child in &tbl_pr.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tblInd")
        {
            return attr_get(el, "w:w").and_then(|v| {
                let twips = match parse_measurement_or_percent(v, "tblInd element") {
                    Ok(MeasurementOrPercent::Number(n)) => n,
                    Ok(MeasurementOrPercent::UniversalTwips(t)) => t,
                    Ok(MeasurementOrPercent::Percent { .. }) => {
                        tracing::warn!(
                            value = %v,
                            "tblInd w:w is a percent form, which has no twips meaning for a \
                             table indent (Word ignores it); dropping the indent"
                        );
                        return None;
                    }
                    Err(e) => {
                        tracing::warn!(
                            value = %v,
                            error = %e.message,
                            "tblInd w:w is not a valid ST_MeasurementOrPercent; dropping the \
                             indent"
                        );
                        return None;
                    }
                };
                match i32::try_from(twips) {
                    Ok(n) => Some(n),
                    Err(_) => {
                        tracing::warn!(
                            value = %v,
                            "tblInd w:w is out of the storable twips range; dropping the indent"
                        );
                        None
                    }
                }
            });
        }
    }
    None
}

/// Parse table layout from w:tblLayout child of a w:tblPr element (§17.4.52).
fn parse_table_layout(tbl_pr: &Element) -> Result<Option<TableLayout>, RuntimeError> {
    for child in &tbl_pr.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tblLayout")
        {
            return match attr_get(el, "w:type") {
                Some(v) => TableLayout::from_xml_str(v)
                    .map(Some)
                    .map_err(|e| invalid_docx(&format!("tblLayout: {e}"))),
                None => Ok(None),
            };
        }
    }
    Ok(None)
}

/// Parse cell spacing from w:tblCellSpacing child of a w:tblPr element (§17.4.44).
///
/// `w:w` is ST_MeasurementOrPercent via CT_TblWidth: plain numbers and
/// universal measures ("0.1in") both resolve to twips, the unit the spacing
/// model stores. OBSERVABLE BOUNDARY for the remaining forms — same
/// rationale as [`parse_table_indent`]: a percent cell spacing has no twips
/// meaning in the model and an invalid value cannot be stored; both are
/// dropped with a warning rather than refusing the import.
fn parse_table_cell_spacing(tbl_pr: &Element) -> Option<i64> {
    for child in &tbl_pr.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tblCellSpacing")
        {
            return attr_get(el, "w:w").and_then(|v| {
                match parse_measurement_or_percent(v, "tblCellSpacing element") {
                    Ok(MeasurementOrPercent::Number(n)) => Some(n),
                    Ok(MeasurementOrPercent::UniversalTwips(t)) => Some(t),
                    Ok(MeasurementOrPercent::Percent { .. }) => {
                        tracing::warn!(
                            value = %v,
                            "tblCellSpacing w:w is a percent form, which has no twips meaning \
                             in the spacing model; dropping the cell spacing"
                        );
                        None
                    }
                    Err(e) => {
                        tracing::warn!(
                            value = %v,
                            error = %e.message,
                            "tblCellSpacing w:w is not a valid ST_MeasurementOrPercent; \
                             dropping the cell spacing"
                        );
                        None
                    }
                }
            });
        }
    }
    None
}

/// Parse floating table positioning from w:tblpPr child of a w:tblPr element (§17.4.57).
fn parse_table_positioning(tbl_pr: &Element) -> Result<Option<TablePositioning>, RuntimeError> {
    for child in &tbl_pr.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tblpPr")
        {
            let vert_anchor = attr_get(el, "w:vertAnchor")
                .map(|v| VAnchor::from_xml_str(v))
                .transpose()
                .map_err(|e| invalid_docx(&format!("tblpPr: {e}")))?;
            let horz_anchor = attr_get(el, "w:horzAnchor")
                .map(|v| HAnchor::from_xml_str(v))
                .transpose()
                .map_err(|e| invalid_docx(&format!("tblpPr: {e}")))?;
            let tblp_x_spec = attr_get(el, "w:tblpXSpec")
                .map(|v| XAlign::from_xml_str(v))
                .transpose()
                .map_err(|e| invalid_docx(&format!("tblpPr: {e}")))?;
            let tblp_y_spec = attr_get(el, "w:tblpYSpec")
                .map(|v| YAlign::from_xml_str(v))
                .transpose()
                .map_err(|e| invalid_docx(&format!("tblpPr: {e}")))?;
            return Ok(Some(TablePositioning {
                vert_anchor,
                horz_anchor,
                tblp_y: attr_get(el, "w:tblpY").and_then(|v| v.parse::<i64>().ok()),
                tblp_x: attr_get(el, "w:tblpX").and_then(|v| v.parse::<i64>().ok()),
                left_from_text: attr_get(el, "w:leftFromText").and_then(|v| v.parse::<i64>().ok()),
                right_from_text: attr_get(el, "w:rightFromText")
                    .and_then(|v| v.parse::<i64>().ok()),
                top_from_text: attr_get(el, "w:topFromText").and_then(|v| v.parse::<i64>().ok()),
                bottom_from_text: attr_get(el, "w:bottomFromText")
                    .and_then(|v| v.parse::<i64>().ok()),
                tblp_x_spec,
                tblp_y_spec,
                // Everything not modeled above is preserved verbatim
                // (§17.4.58 CT_TblPPr remainder).
                extra_attrs: crate::xml_attrs::capture_extra_attrs(
                    el,
                    &[
                        "vertAnchor",
                        "horzAnchor",
                        "tblpY",
                        "tblpX",
                        "leftFromText",
                        "rightFromText",
                        "topFromText",
                        "bottomFromText",
                        "tblpXSpec",
                        "tblpYSpec",
                    ],
                ),
            }));
        }
    }
    Ok(None)
}

/// Parse table overlap from w:tblOverlap child of a w:tblPr element (§17.4.55).
fn parse_table_overlap(tbl_pr: &Element) -> Result<Option<TableOverlap>, RuntimeError> {
    for child in &tbl_pr.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tblOverlap")
        {
            return match attr_get(el, "w:val") {
                Some(v) => TableOverlap::from_xml_str(v)
                    .map(Some)
                    .map_err(|e| invalid_docx(&format!("tblOverlap: {e}"))),
                None => Ok(None),
            };
        }
    }
    Ok(None)
}
/// Parse band size from tblPr (tblStyleRowBandSize or tblStyleColBandSize).
fn parse_band_size(tbl_pr: &Element, tag: &str) -> Option<u32> {
    for child in &tbl_pr.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, tag)
        {
            return attr_get(el, "w:val").and_then(|v| v.parse::<u32>().ok());
        }
    }
    None
}

/// Parse a border set from a parent element (e.g., tblBorders or tcBorders).
///
/// Looks for a child element matching `border_element_name` and extracts
/// top/bottom/left/right/insideH/insideV borders from it.
fn parse_border_set(
    parent: &Element,
    border_element_name: &str,
) -> Result<Option<BorderSet>, RuntimeError> {
    for child in &parent.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, border_element_name)
        {
            let left = match parse_border_edge(el, "left")? {
                Some(b) => Some(b),
                None => parse_border_edge(el, "start")?,
            };
            let right = match parse_border_edge(el, "right")? {
                Some(b) => Some(b),
                None => parse_border_edge(el, "end")?,
            };
            return Ok(Some(BorderSet {
                top: parse_border_edge(el, "top")?,
                bottom: parse_border_edge(el, "bottom")?,
                left,
                right,
                inside_h: parse_border_edge(el, "insideH")?,
                inside_v: parse_border_edge(el, "insideV")?,
            }));
        }
    }
    Ok(None)
}

/// Parse a single border edge from a border set element.
fn parse_border_edge(
    borders_el: &Element,
    edge_name: &str,
) -> Result<Option<Border>, RuntimeError> {
    for child in &borders_el.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, edge_name)
        {
            let style_str = attr_get(el, "w:val").map(|s| s.as_str()).unwrap_or("none");
            let style = BorderStyle::from_xml_str(style_str)
                .map_err(|e| invalid_docx(&format!("parse_border_edge({edge_name}): {e}")))?;
            let color = attr_get(el, "w:color").cloned();
            // Unlike tblInd/tblCellSpacing (CT_TblWidth, a union that also
            // admits universal-measure/percent forms), border w:sz
            // (ST_EighthPointMeasure) and w:space (ST_PointMeasure) are plain
            // unbounded unsigned decimals (confirmed by
            // spec_para_borders_shading_word_compliance.rs's
            // ST_EighthPointMeasure test) — no unit-suffix ambiguity, so a
            // present-but-unparseable value really is malformed and can
            // fail fast, matching parse_table_measurement's w:tblW/w:tcW
            // pattern (reusing its tolerant int-or-float parse).
            let size = attr_get(el, "w:sz")
                .map(|s| parse_twips(s, &format!("{edge_name} border w:sz")))
                .transpose()?;
            let space = attr_get(el, "w:space")
                .map(|s| parse_twips(s, &format!("{edge_name} border w:space")))
                .transpose()?;
            return Ok(Some(Border {
                style,
                color,
                size,
                space,
                // Preserve theme colors / frame / shadow verbatim (RFC-0003).
                extra_attrs: crate::xml_attrs::capture_extra_attrs(
                    el,
                    &["val", "sz", "space", "color"],
                ),
            }));
        }
    }
    Ok(None)
}

/// Parse a shading element (w:shd) from a properties element.
fn parse_shading(props: &Element) -> Result<Option<Shading>, RuntimeError> {
    for child in &props.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "shd")
        {
            let fill = attr_get(el, "w:fill").cloned();
            let val = attr_get(el, "w:val")
                .map(|v| ShadingPattern::from_xml_str(v))
                .transpose()
                .map_err(|e| invalid_docx(&format!("shading: {e}")))?;
            let color = attr_get(el, "w:color").cloned();
            // Preserve theme fills/colors verbatim (RFC-0003).
            let extra_attrs = crate::xml_attrs::capture_extra_attrs(el, &["val", "fill", "color"]);
            // Only return shading if there's meaningful data
            if fill.is_some() || val.is_some() || color.is_some() || !extra_attrs.is_empty() {
                return Ok(Some(Shading {
                    fill,
                    val,
                    color,
                    extra_attrs,
                }));
            }
        }
    }
    Ok(None)
}

/// Convert a word_ir `BorderEdge` to a domain `Border`.
fn border_edge_to_domain(edge: crate::word_ir::BorderEdge) -> Result<Border, RuntimeError> {
    let style = BorderStyle::from_xml_str(&edge.style)
        .map_err(|e| invalid_docx(&format!("border_edge_to_domain: {e}")))?;
    Ok(Border {
        style,
        color: edge.color,
        size: edge.size,
        space: edge.space,
        extra_attrs: Vec::new(),
    })
}
/// Convert resolved paragraph border edges to domain ParagraphBorders.
fn convert_paragraph_borders_from_edges(
    resolved_borders: Option<crate::word_ir::ParagraphBorderProps>,
) -> Result<Option<ParagraphBorders>, RuntimeError> {
    match resolved_borders {
        Some(b) => Ok(Some(ParagraphBorders {
            top: b.top.map(border_edge_to_domain).transpose()?,
            bottom: b.bottom.map(border_edge_to_domain).transpose()?,
            left: b.left.map(border_edge_to_domain).transpose()?,
            right: b.right.map(border_edge_to_domain).transpose()?,
            between: b.between.map(border_edge_to_domain).transpose()?,
            bar: b.bar.map(border_edge_to_domain).transpose()?,
        })),
        None => Ok(None),
    }
}

/// Parse the `w:id` attribute from a tracked change element (w:ins, w:del).
///
/// Returns a `RuntimeError` if `w:id` is missing or not a valid u32,
/// since a tracked change without a revision ID corrupts Word's accept/reject UI.
fn parse_revision_id(el: &Element, element_name: &str) -> Result<u32, RuntimeError> {
    let raw = attr_get(el, "w:id").ok_or_else(|| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!(
            "Tracked change element <{element_name}> is missing required w:id attribute"
        ),
        details: ErrorDetails::default(),
    })?;
    raw.parse::<u32>().map_err(|_| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!(
            "Tracked change element <{element_name}> has non-numeric w:id attribute: '{raw}'"
        ),
        details: ErrorDetails::default(),
    })
}
/// Parse cell margins from a container element (e.g., w:tcMar for per-cell, w:tblCellMar for table-level).
fn parse_cell_margins(parent: &Element, container_name: &str) -> Option<CellMargins> {
    // Find the margin container child (e.g., tcMar or tblCellMar)
    let container = parent.children.iter().find_map(|child| {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, container_name)
        {
            return Some(el);
        }
        None
    })?;

    let margin_value = |name: &str| -> Option<u32> {
        container.children.iter().find_map(|child| {
            if let XMLNode::Element(el) = child
                && is_w_tag(el, name)
            {
                // MS-OI29500 §17.4.42: type="nil" means "not specified" — skip element.
                if attr_get(el, "w:type").map(|v| v.as_str()) == Some("nil") {
                    return None;
                }
                // The margin elements are CT_TblWidth, so w:w is
                // ST_MeasurementOrPercent: plain numbers and universal
                // measures resolve to twips (the model's unit). OBSERVABLE
                // BOUNDARY — same rationale as parse_table_indent: percent /
                // negative / invalid values cannot be stored as an unsigned
                // twips margin and are dropped with a warning.
                return attr_get(el, "w:w").and_then(|v| {
                    let twips = match parse_measurement_or_percent(
                        v,
                        &format!("{container_name} {name}"),
                    ) {
                        Ok(MeasurementOrPercent::Number(n)) => n,
                        Ok(MeasurementOrPercent::UniversalTwips(t)) => t,
                        Ok(MeasurementOrPercent::Percent { .. }) | Err(_) => {
                            tracing::warn!(
                                value = %v,
                                container = container_name,
                                edge = name,
                                "cell margin w:w is not storable as twips (percent or \
                                 invalid form); dropping the margin"
                            );
                            return None;
                        }
                    };
                    match u32::try_from(twips) {
                        Ok(n) => Some(n),
                        Err(_) => {
                            tracing::warn!(
                                value = %v,
                                container = container_name,
                                edge = name,
                                "cell margin w:w is negative or out of range; dropping the margin"
                            );
                            None
                        }
                    }
                });
            }
            None
        })
    };

    let top = margin_value("top");
    let bottom = margin_value("bottom");
    // "start" is the newer alias for "left" in OOXML
    let left = margin_value("left").or_else(|| margin_value("start"));
    // "end" is the newer alias for "right" in OOXML
    let right = margin_value("right").or_else(|| margin_value("end"));

    if top.is_none() && bottom.is_none() && left.is_none() && right.is_none() {
        return None;
    }

    Some(CellMargins {
        top,
        bottom,
        left,
        right,
    })
}

/// Parse a string as a twip (twentieths of a point) integer value.
///
/// Twips are integer units per the OOXML spec, but some real-world producers
/// emit float values like "9634.0". We parse as f64 first and truncate to u32
/// when the string isn't a valid integer. This is a deliberate "parse tolerantly,
/// store precisely" decision at the import edge.
fn parse_twips(s: &str, context: &str) -> Result<u32, RuntimeError> {
    // Fast path: try integer parse first (the common case).
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }
    // Slow path: try float parse for values like "9634.0".
    let f: f64 = s.parse().map_err(|_| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("invalid width value '{s}' in {context}"),
        details: ErrorDetails::default(),
    })?;
    if f < 0.0 || f > u32::MAX as f64 {
        return Err(RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!("width value '{s}' out of range in {context}"),
            details: ErrorDetails::default(),
        });
    }
    Ok(f as u32)
}

/// Parse an ST_TwipsMeasure value (§22.9.2.14): an unsigned decimal number
/// of twips OR a positive universal measure ("1in", "0.5cm"), used by
/// `w:gridCol/@w` and `w:trHeight/@val`. Universal measures convert to
/// twips (rounded); everything else follows [`parse_twips`], including its
/// fail-fast on non-numeric values.
fn parse_twips_measure(s: &str, context: &str) -> Result<u32, RuntimeError> {
    if let Ok(MeasurementOrPercent::UniversalTwips(t)) = parse_measurement_or_percent(s, context) {
        return u32::try_from(t).map_err(|_| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!("width value '{s}' out of range in {context}"),
            details: ErrorDetails::default(),
        });
    }
    parse_twips(s, context)
}

/// A `w:w` attribute value on a CT_TblWidth carrier (`w:tblW`, `w:tcW`,
/// `w:tblInd`, `w:tblCellSpacing`, `w:wBefore`, `w:wAfter`, `w:tcMar`
/// children), parsed per its schema type ST_MeasurementOrPercent
/// (ECMA-376 §17.18.107): ST_DecimalNumberOrPercent ∪ ST_UniversalMeasure.
///
/// The three source forms are kept distinct because their unit semantics
/// differ: a plain number's unit is decided by the *sibling* `w:type`
/// attribute, while a percent literal or universal measure carries its unit
/// in the value itself (and therefore wins over a contradictory `w:type`,
/// matching how Word consumes such files).
#[derive(Debug, PartialEq)]
pub(crate) enum MeasurementOrPercent {
    /// Plain decimal number (tolerant of float spellings like "9634.0");
    /// unit depends on the declared `w:type` (dxa → twips, pct → fiftieths
    /// of a percent). Negative values are accepted here — carriers whose
    /// model is unsigned range-check at their own edge.
    Number(i64),
    /// ST_Percentage literal (`-?[0-9]+(\.[0-9]+)?%`, e.g. "33.3%").
    /// `fiftieths` is the value normalized to fiftieths of a percent
    /// (the unit plain-number pct widths use); `literal` is the exact
    /// source spelling, preserved for source-form-faithful re-emission.
    Percent { fiftieths: i64, literal: String },
    /// ST_UniversalMeasure (`-?[0-9]+(\.[0-9]+)?(mm|cm|in|pt|pc|pi)`,
    /// e.g. "1.5in"), converted to twips (rounded to nearest).
    UniversalTwips(i64),
}

/// Check the numeric part of an ST_Percentage / ST_UniversalMeasure value:
/// `-?[0-9]+(\.[0-9]+)?`. Stricter than `f64::parse` (no exponents, no
/// "inf"/"nan", no leading '+' or '.'), matching the schema patterns.
fn is_schema_decimal(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (digits, None),
    };
    let all_digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    all_digits(int_part) && frac_part.is_none_or(all_digits)
}

/// Twips per unit for the six ST_UniversalMeasure units (§22.9.2.15).
/// 1in = 1440 twips; 1pt = 20 twips; 1pc = 1pi = 12pt; 25.4mm = 1in.
fn universal_measure_unit_to_twips(unit: &str) -> Option<f64> {
    match unit {
        "mm" => Some(1440.0 / 25.4),
        "cm" => Some(1440.0 / 2.54),
        "in" => Some(1440.0),
        "pt" => Some(20.0),
        "pc" | "pi" => Some(240.0),
        _ => None,
    }
}

/// Parse an ST_MeasurementOrPercent attribute value (§17.18.107).
///
/// Fail-fast: a value matching none of the three legal forms is an
/// InvalidDocx error naming the value and the carrier element.
pub(crate) fn parse_measurement_or_percent(
    s: &str,
    context: &str,
) -> Result<MeasurementOrPercent, RuntimeError> {
    let invalid = || RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("invalid width value '{s}' in {context}"),
        details: ErrorDetails::default(),
    };
    let out_of_range = || RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("width value '{s}' out of range in {context}"),
        details: ErrorDetails::default(),
    };
    // ST_Percentage: "-?N(.N)?%".
    if let Some(num) = s.strip_suffix('%') {
        if !is_schema_decimal(num) {
            return Err(invalid());
        }
        let pct: f64 = num.parse().map_err(|_| invalid())?;
        let fiftieths = (pct * 50.0).round();
        if !(i64::MIN as f64..=i64::MAX as f64).contains(&fiftieths) {
            return Err(out_of_range());
        }
        return Ok(MeasurementOrPercent::Percent {
            fiftieths: fiftieths as i64,
            literal: s.to_string(),
        });
    }
    // ST_UniversalMeasure: "-?N(.N)?(mm|cm|in|pt|pc|pi)".
    if s.len() > 2 && s.is_char_boundary(s.len() - 2) {
        let (num, unit) = s.split_at(s.len() - 2);
        if let Some(twips_per_unit) = universal_measure_unit_to_twips(unit) {
            if !is_schema_decimal(num) {
                return Err(invalid());
            }
            let n: f64 = num.parse().map_err(|_| invalid())?;
            let twips = (n * twips_per_unit).round();
            if !(i64::MIN as f64..=i64::MAX as f64).contains(&twips) {
                return Err(out_of_range());
            }
            return Ok(MeasurementOrPercent::UniversalTwips(twips as i64));
        }
    }
    // ST_DecimalNumber — integer per the schema, but some real-world
    // producers emit float spellings like "9634.0"; parse tolerantly and
    // truncate, same policy as parse_twips.
    if let Ok(n) = s.parse::<i64>() {
        return Ok(MeasurementOrPercent::Number(n));
    }
    let f: f64 = s.parse().map_err(|_| invalid())?;
    if !(i64::MIN as f64..=i64::MAX as f64).contains(&f) {
        return Err(out_of_range());
    }
    Ok(MeasurementOrPercent::Number(f as i64))
}

/// Parse a table measurement (e.g., w:tblW, w:tcW) from a properties element.
///
/// `w:w` is ST_MeasurementOrPercent (§17.18.107): besides a plain number it
/// legally carries a percent literal ("100%", "40.0%") or a universal measure
/// ("1.5in") — wild Word-authored documents use the percent form and Word
/// opens them without repair. A percent literal is normalized to fiftieths of
/// a percent with the source spelling kept on the measurement; a universal
/// measure is normalized to twips. In both cases the value's own unit
/// overrides a contradictory declared `w:type` (Word ignores the type, never
/// treats the file as corrupt).
fn parse_table_measurement(
    props: &Element,
    element_name: &str,
) -> Result<Option<TableMeasurement>, RuntimeError> {
    for child in &props.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, element_name)
        {
            let context = format!("{element_name} element");
            let out_of_range = |s: &str| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!("width value '{s}' out of range in {context}"),
                details: ErrorDetails::default(),
            };
            let to_u32 = |n: i64, s: &str| -> Result<u32, RuntimeError> {
                u32::try_from(n).map_err(|_| out_of_range(s))
            };
            let width_type_str = attr_get(el, "w:type").map(|s| s.as_str()).unwrap_or("dxa");
            let declared_type =
                WidthType::from_xml_str(width_type_str).map_err(|e| RuntimeError {
                    code: ErrorCode::InvalidDocx,
                    message: format!("invalid width type in {element_name} element: {e}"),
                    details: ErrorDetails::default(),
                })?;
            let (w, mut width_type, pct_literal) = match attr_get(el, "w:w") {
                None => (0, declared_type, None),
                Some(s) => match parse_measurement_or_percent(s, &context)? {
                    MeasurementOrPercent::Number(n) => (to_u32(n, s)?, declared_type, None),
                    MeasurementOrPercent::Percent { fiftieths, literal } => {
                        (to_u32(fiftieths, s)?, WidthType::Pct, Some(literal))
                    }
                    MeasurementOrPercent::UniversalTwips(t) => {
                        (to_u32(t, s)?, WidthType::Dxa, None)
                    }
                },
            };
            // MS-OI29500 §2.1.166: If w=0, treat type as "auto" regardless of
            // declared value — but preserve Nil, which is a distinct semantic
            // ("no width specified") per §17.18.90 ST_TblWidth, and preserve
            // an explicit percent literal ("0%"), whose form the value itself
            // fixes as pct.
            if w == 0
                && pct_literal.is_none()
                && width_type != WidthType::Nil
                && width_type != WidthType::Auto
            {
                width_type = WidthType::Auto;
            }
            return Ok(Some(TableMeasurement {
                w,
                width_type,
                pct_literal,
            }));
        }
    }
    Ok(None)
}

/// Parse a single `w:cnfStyle` element (§17.4.7 / §17.3.1.8) into a CnfStyle.
///
/// Used for the row-level (trPr) and cell-level (tcPr) conditional-formatting
/// flags. Paragraph-level cnfStyle is parsed independently in word_ir.rs; both
/// produce the same domain type. The 12 boolean attributes mirror the bits of
/// the legacy `w:val` 12-character binary string, which is preserved verbatim.
fn parse_cnf_style(cnf_el: &Element) -> Option<crate::domain::CnfStyle> {
    let bool_attr = |name: &str| -> bool {
        matches!(
            attr_get(cnf_el, name).map(|s| s.as_str()),
            Some("1") | Some("true")
        )
    };
    Some(crate::domain::CnfStyle {
        val: attr_get(cnf_el, "w:val").cloned(),
        first_row: bool_attr("w:firstRow"),
        last_row: bool_attr("w:lastRow"),
        first_column: bool_attr("w:firstColumn"),
        last_column: bool_attr("w:lastColumn"),
        odd_v_band: bool_attr("w:oddVBand"),
        even_v_band: bool_attr("w:evenVBand"),
        odd_h_band: bool_attr("w:oddHBand"),
        even_h_band: bool_attr("w:evenHBand"),
        first_row_first_column: bool_attr("w:firstRowFirstColumn"),
        first_row_last_column: bool_attr("w:firstRowLastColumn"),
        last_row_first_column: bool_attr("w:lastRowFirstColumn"),
        last_row_last_column: bool_attr("w:lastRowLastColumn"),
    })
}

/// Parse a `w:tblPrEx` element (§17.4.61, CT_TblPrEx) into a TableFormatting.
///
/// tblPrEx carries per-row overrides of table-level properties. Only the
/// CT_TblPrEx subset is meaningful (borders, shading, width, cell margins,
/// alignment, indent, layout, cell spacing, tblLook); fields outside that
/// subset stay at their default. The same per-property parsers used for w:tblPr
/// are reused so nested tblBorders/insideH/insideV round-trip for free.
fn parse_tbl_pr_ex(el: &Element) -> Result<TableFormatting, RuntimeError> {
    // Only carry tblLook when one is actually present — parse_tbl_look returns
    // the MS 0x04A0 default for absent elements, which we must not inject.
    let has_tbl_look = el
        .children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(e) if is_w_tag(e, "tblLook")));
    let fmt = TableFormatting {
        borders: parse_border_set(el, "tblBorders")?,
        width: parse_table_measurement(el, "tblW")?,
        default_cell_margins: parse_cell_margins(el, "tblCellMar"),
        alignment: parse_table_alignment(el),
        indent: parse_table_indent(el),
        layout: parse_table_layout(el)?,
        cell_spacing: parse_table_cell_spacing(el),
        tbl_look: has_tbl_look.then(|| parse_tbl_look(el)),
        ..Default::default()
    };
    Ok(fmt)
}

/// Parse grid column widths from a w:tblGrid element.
fn parse_grid_cols(tbl_grid: &Element) -> Result<Vec<u32>, RuntimeError> {
    let mut cols = Vec::new();
    for child in &tbl_grid.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "gridCol")
        {
            let w = match attr_get(el, "w:w") {
                Some(s) => parse_twips_measure(s, "gridCol element")?,
                None => 0,
            };
            cols.push(w);
        }
    }
    Ok(cols)
}
// =============================================================================
// Table Conditional Formatting (§17.7.6)
// =============================================================================

// TblLook is now defined in domain.rs and imported above.

/// Parse w:tblLook from within a w:tblPr element.
///
/// MS-OI29500 §17.4.55(c): Word reads individual attributes (firstRow, lastRow, etc.)
/// if any are present. If none are present, falls back to the w:val bitmask:
/// 0x0020=firstRow, 0x0040=lastRow, 0x0080=firstCol, 0x0100=lastCol,
/// 0x0200=noHBand, 0x0400=noVBand.
fn parse_tbl_look(tbl_pr: &Element) -> TblLook {
    for child in &tbl_pr.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tblLook")
        {
            // Capture raw w:val for roundtrip fidelity.
            let raw_val = attr_get(el, "w:val").cloned();

            // Check if any individual attributes are present.
            let has_individual = attr_get(el, "w:firstRow").is_some()
                || attr_get(el, "w:lastRow").is_some()
                || attr_get(el, "w:firstColumn").is_some()
                || attr_get(el, "w:lastColumn").is_some()
                || attr_get(el, "w:noHBand").is_some()
                || attr_get(el, "w:noVBand").is_some();

            if has_individual {
                let bit = |attr: &str| -> bool {
                    attr_get(el, attr)
                        .map(|v| v == "1" || v == "true")
                        .unwrap_or(false)
                };
                return TblLook {
                    first_row: bit("w:firstRow"),
                    last_row: bit("w:lastRow"),
                    first_column: bit("w:firstColumn"),
                    last_column: bit("w:lastColumn"),
                    no_h_band: bit("w:noHBand"),
                    no_v_band: bit("w:noVBand"),
                    val: raw_val,
                };
            }

            // Fallback: parse w:val bitmask.
            if let Some(val_str) = attr_get(el, "w:val")
                && let Ok(val) = u32::from_str_radix(val_str, 16)
            {
                return TblLook {
                    first_row: val & 0x0020 != 0,
                    last_row: val & 0x0040 != 0,
                    first_column: val & 0x0080 != 0,
                    last_column: val & 0x0100 != 0,
                    no_h_band: val & 0x0200 != 0,
                    no_v_band: val & 0x0400 != 0,
                    val: raw_val,
                };
            }

            // tblLook element present but no parseable attributes or val:
            // treat as all-false (element is present but empty).
            return TblLook {
                first_row: false,
                last_row: false,
                first_column: false,
                last_column: false,
                no_h_band: false,
                no_v_band: false,
                val: raw_val,
            };
        }
    }
    // No tblLook element → MS default 0x04A0.
    TblLook::default()
}

/// Apply conditional formatting from a table style to cells as a post-processing step.
///
/// For each cell, determines which conditions apply based on position and tblLook flags,
/// then fills in any `None` formatting fields from the matching conditional overrides.
/// Precedence: direct formatting > conditional > base style default_cell_shading.
#[allow(clippy::too_many_arguments)]
fn apply_conditional_formatting(
    rows: &mut [TableRowNode],
    conditional: &std::collections::HashMap<
        crate::styles::TblStylePrType,
        crate::styles::ConditionalCellProps,
    >,
    tbl_look: &TblLook,
    default_cell_shading: &Option<Shading>,
    row_band_size: u32,
    col_band_size: u32,
    // Base run props from style root-level rPr — applied as lowest-precedence fallback
    // when no conditional sets the respective property.
    base_bold: Option<bool>,
    base_color: Option<&IStr>,
    base_font_family: Option<&IStr>,
) {
    use crate::styles::TblStylePrType;

    let total_rows = rows.len();
    let total_cols = rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.grid_span).sum::<u32>())
        .max()
        .unwrap_or(0) as usize;

    for (row_idx, row) in rows.iter_mut().enumerate() {
        // Determine which column each cell starts at.
        let mut col_idx: usize = 0;
        let num_cells = row.cells.len();
        // ISO 29500-1 §17.18.89 (firstRow): "Any subsequent row which has the
        // tblHeader element present (§17.4.49) shall also use this conditional
        // format." So the firstRow REGION extends to repeated-header rows — but
        // only the firstRow conditional; the literal table corners (nw/ne cells)
        // and band counting still key off row 0.
        let row_is_header = row.is_header;

        for cell_pos in 0..num_cells {
            let cell_col = col_idx;
            let cell_span = row.cells[cell_pos].grid_span as usize;

            // Determine which conditions match this cell.
            let is_first_row = row_idx == 0 && tbl_look.first_row;
            // The firstRow conditional format also covers repeated-header rows
            // (§17.18.89), unlike the literal-corner / banding logic below.
            let is_first_row_region = is_first_row || (row_is_header && tbl_look.first_row);
            let is_last_row = row_idx == total_rows - 1 && tbl_look.last_row;
            let is_first_col = cell_col == 0 && tbl_look.first_column;
            let is_last_col = cell_col + cell_span >= total_cols && tbl_look.last_column;

            // Data row index: excludes firstRow/lastRow from banding calculation.
            let data_row_idx = if tbl_look.first_row && row_idx > 0 {
                row_idx - 1
            } else {
                row_idx
            };

            // Data col index: excludes firstCol/lastCol from banding calculation.
            let data_col_idx = if tbl_look.first_column && cell_col > 0 {
                cell_col - 1
            } else {
                cell_col
            };

            // Collect matching condition types in precedence order (highest first).
            // MS-OI29500 §17.7.6(c): Office applies conditional formats in this order
            // (later overrides earlier): wholeTable < bands < firstCol/lastCol
            // < firstRow/lastRow < corners.
            // Since we use fill-first-wins, push highest precedence first.
            let mut matching: Vec<&TblStylePrType> = Vec::new();

            // Highest precedence: corners (nwCell/neCell/swCell/seCell)
            // MS-OI29500 §17.4.54(a): Corner cells override firstRow/lastRow/firstCol/lastCol.
            if is_first_row && is_first_col {
                matching.push(&TblStylePrType::NwCell);
            }
            if is_first_row && is_last_col {
                matching.push(&TblStylePrType::NeCell);
            }
            if is_last_row && is_first_col {
                matching.push(&TblStylePrType::SwCell);
            }
            if is_last_row && is_last_col {
                matching.push(&TblStylePrType::SeCell);
            }

            // Next: firstRow/lastRow (higher than firstCol/lastCol per MS-OI29500).
            // firstRow also applies to repeated-header rows (§17.18.89).
            if is_first_row_region {
                matching.push(&TblStylePrType::FirstRow);
            }
            if is_last_row {
                matching.push(&TblStylePrType::LastRow);
            }

            // Next: firstCol/lastCol
            if is_first_col {
                matching.push(&TblStylePrType::FirstCol);
            }
            if is_last_col {
                matching.push(&TblStylePrType::LastCol);
            }

            // Band rows (§17.7.6 level 3) — only if not in first/last row
            // MS-OI29500 §17.7.6.7: band size 0 means no banding.
            if !tbl_look.no_h_band && !is_first_row && !is_last_row && row_band_size > 0 {
                if (data_row_idx / row_band_size as usize).is_multiple_of(2) {
                    matching.push(&TblStylePrType::Band1Horz);
                } else {
                    matching.push(&TblStylePrType::Band2Horz);
                }
            }

            // Band columns (§17.7.6 level 2) — only if not in first/last col
            // MS-OI29500 §17.7.6.5: band size 0 means no banding.
            if !tbl_look.no_v_band && !is_first_col && !is_last_col && col_band_size > 0 {
                if (data_col_idx / col_band_size as usize).is_multiple_of(2) {
                    matching.push(&TblStylePrType::Band1Vert);
                } else {
                    matching.push(&TblStylePrType::Band2Vert);
                }
            }

            // MS-OI29500 §2.1.557 (§17.18.89): Word does not apply and discards on
            // save any properties within tblStylePr when type="wholeTable".
            // Root-level style properties (pPr/rPr/tcPr) are handled separately
            // via base_* fields and default_cell_shading.

            // Apply matching conditionals: fill None fields from first match.
            let cell = &mut row.cells[cell_pos];
            let mut cond_alignment: Option<Alignment> = None;
            let mut cond_bold: Option<bool> = None;
            let mut cond_font_size: Option<u32> = None;
            let mut cond_font_family: Option<IStr> = None;
            let mut cond_color: Option<IStr> = None;
            for cond_type in &matching {
                if let Some(cond_props) = conditional.get(cond_type) {
                    if cell.formatting.shading.is_none() && cond_props.shading.is_some() {
                        cell.formatting.shading = cond_props.shading.clone();
                    }
                    if cell.formatting.borders.is_none() && cond_props.borders.is_some() {
                        cell.formatting.borders = cond_props.borders.clone();
                    }
                    if cell.formatting.margins.is_none() && cond_props.margins.is_some() {
                        cell.formatting.margins = cond_props.margins.clone();
                    }
                    if cond_alignment.is_none() && cond_props.alignment.is_some() {
                        cond_alignment = cond_props.alignment.clone();
                    }
                    if cond_bold.is_none() && cond_props.bold.is_some() {
                        cond_bold = cond_props.bold;
                    }
                    if cond_font_size.is_none() && cond_props.font_size.is_some() {
                        cond_font_size = cond_props.font_size;
                    }
                    if cond_font_family.is_none() && cond_props.font_family.is_some() {
                        cond_font_family = cond_props.font_family.clone();
                    }
                    if cond_color.is_none() && cond_props.color.is_some() {
                        cond_color = cond_props.color.clone();
                    }
                }
            }

            // Fall back to base style root-level rPr values when no conditional
            // sets the respective property. Base props sit below all conditionals
            // in the precedence chain (§17.7.6).
            if cond_bold.is_none() {
                cond_bold = base_bold;
            }
            if cond_color.is_none() {
                cond_color = base_color.cloned();
            }
            if cond_font_family.is_none() {
                cond_font_family = base_font_family.cloned();
            }

            // §17.7.6.1: propagate conditional pPr alignment to paragraphs.
            // Direct paragraph alignment has higher precedence than conditional per §17.7.6.
            if let Some(ref align) = cond_alignment {
                for block in &mut cell.blocks {
                    if let BlockNode::Paragraph(para) = block
                        && !para.has_direct_align
                    {
                        para.align = Some(align.clone());
                    }
                }
            }

            // §17.7.6.2: propagate conditional rPr bold to text runs (if not already bold).
            if cond_bold == Some(true) {
                for block in &mut cell.blocks {
                    if let BlockNode::Paragraph(para) = block {
                        for seg in &mut para.segments {
                            for inline in &mut seg.inlines {
                                if let InlineNode::Text(text) = inline
                                    && !text.marks.contains(&Mark::Bold)
                                {
                                    text.marks.push(Mark::Bold);
                                }
                            }
                        }
                    }
                }
            }

            // §17.7.6.2: propagate conditional rPr font size to text runs.
            // Direct run font size has higher precedence than conditional per §17.7.6.
            if let Some(size) = cond_font_size {
                for block in &mut cell.blocks {
                    if let BlockNode::Paragraph(para) = block {
                        for seg in &mut para.segments {
                            for inline in &mut seg.inlines {
                                if let InlineNode::Text(text) = inline
                                    && !text.rpr_authored.font_size
                                {
                                    text.style_props.font_size = Some(size);
                                }
                            }
                        }
                    }
                }
            }

            // §17.7.6.2: propagate conditional rPr font family to text runs.
            // Direct run font family has higher precedence than conditional per §17.7.6.
            if let Some(ref font) = cond_font_family {
                for block in &mut cell.blocks {
                    if let BlockNode::Paragraph(para) = block {
                        for seg in &mut para.segments {
                            for inline in &mut seg.inlines {
                                if let InlineNode::Text(text) = inline
                                    && !text.rpr_authored.font_family_any()
                                {
                                    text.style_props.font_family = Some(font.clone());
                                }
                            }
                        }
                    }
                }
            }

            // §17.7.6.2: propagate conditional rPr color to text runs.
            // Direct run color has higher precedence than conditional per §17.7.6.
            if let Some(ref color) = cond_color {
                for block in &mut cell.blocks {
                    if let BlockNode::Paragraph(para) = block {
                        for seg in &mut para.segments {
                            for inline in &mut seg.inlines {
                                if let InlineNode::Text(text) = inline
                                    && !text.rpr_authored.color_any()
                                {
                                    text.style_props.color = Some(color.clone());
                                }
                            }
                        }
                    }
                }
            }

            // Final fallback: base style default cell shading.
            if cell.formatting.shading.is_none() {
                cell.formatting.shading = default_cell_shading.clone();
            }

            col_idx += cell_span;
        }
    }
}

/// Apply a table style's base paragraph alignment and font size to cell paragraphs
/// when the overrideTableStyleFontSizeAndJustification compat setting is false/absent
/// (MS-DOCX §2.3.1).
///
/// Per the spec, when this setting is false (the default), the default paragraph style's
/// (typically Normal) font size and left justification do NOT override the table style's
/// values. Only paragraphs using the default paragraph style (or no explicit style) are
/// affected, and only when they have no direct formatting for the relevant property.
fn apply_table_style_base_props(
    rows: &mut [TableRowNode],
    base_alignment: Option<&Alignment>,
    base_font_size: Option<u32>,
    default_para_style_id: &str,
) {
    if base_alignment.is_none() && base_font_size.is_none() {
        return;
    }

    for row in rows.iter_mut() {
        for cell in &mut row.cells {
            for block in &mut cell.blocks {
                if let BlockNode::Paragraph(para) = block {
                    // Only override paragraphs using the default paragraph style.
                    // style_id is None for unstyled paragraphs (which implicitly use
                    // the default), or explicitly set to the default style ID.
                    let uses_default_style = para.style_id.is_none()
                        || para.style_id.as_deref() == Some(default_para_style_id);
                    if !uses_default_style {
                        continue;
                    }

                    // Override alignment if the paragraph doesn't have direct jc.
                    if let Some(align) = base_alignment
                        && !para.has_direct_align
                    {
                        para.align = Some(align.clone());
                    }

                    // Override font size on runs that don't have direct sz.
                    if let Some(size) = base_font_size {
                        for seg in &mut para.segments {
                            for inline in &mut seg.inlines {
                                if let InlineNode::Text(text) = inline
                                    && !text.rpr_authored.font_size
                                {
                                    text.style_props.font_size = Some(size);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Apply a table style's base run properties (bold, color, font_family) from the style's
/// root-level w:rPr to text runs in all cells.
///
/// These are NOT gated by overrideTableStyleFontSizeAndJustification — they always apply
/// as table-style defaults. Only runs without direct formatting for the respective property
/// are affected.
fn apply_table_style_base_run_props(
    rows: &mut [TableRowNode],
    base_bold: Option<bool>,
    base_color: Option<&IStr>,
    base_font_family: Option<&IStr>,
) {
    if base_bold.is_none() && base_color.is_none() && base_font_family.is_none() {
        return;
    }

    for row in rows.iter_mut() {
        for cell in &mut row.cells {
            for block in &mut cell.blocks {
                if let BlockNode::Paragraph(para) = block {
                    for seg in &mut para.segments {
                        for inline in &mut seg.inlines {
                            if let InlineNode::Text(text) = inline {
                                if base_bold == Some(true) && !text.marks.contains(&Mark::Bold) {
                                    text.marks.push(Mark::Bold);
                                }
                                if let Some(color) = base_color
                                    && !text.rpr_authored.color_any()
                                {
                                    text.style_props.color = Some(color.clone());
                                }
                                if let Some(font) = base_font_family
                                    && !text.rpr_authored.font_family_any()
                                {
                                    text.style_props.font_family = Some(font.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Compute the weight of a border for conflict resolution (MS-OI29500 §2.1.169).
///
/// Weight = border_width × border_number, where border_number is the
/// spec-defined rank (1 = single through 23 = inset).
///
/// Special cases per §2.1.169:
/// - none/nil: weight 0
/// - dotted/dashed: always weight 1 regardless of border width and number
fn border_weight(border: &Border) -> u32 {
    match border.style {
        BorderStyle::None | BorderStyle::Nil => return 0,
        // MS-OI29500 §2.1.169: "The borders with dotted and dashed styles
        // shall be assigned the weight 1 regardless of the border width
        // and number."
        BorderStyle::Dotted | BorderStyle::Dashed => return 1,
        _ => {}
    }
    let size = border.size.unwrap_or(0);
    // Border number per the explicit table in MS-OI29500 §2.1.169. The numbers
    // are NOT sequential — the spec skips 4-7 and 23 — and this is what Word
    // actually applies when resolving adjacent-cell border conflicts.
    let border_number: u32 = match border.style {
        BorderStyle::Single => 1,
        BorderStyle::Thick => 2,
        BorderStyle::Double => 3,
        BorderStyle::DotDash => 8,
        BorderStyle::DotDotDash => 9,
        BorderStyle::Triple => 10,
        BorderStyle::ThinThickSmallGap => 11,
        BorderStyle::ThickThinSmallGap => 12,
        BorderStyle::ThinThickThinSmallGap => 13,
        BorderStyle::ThinThickMediumGap => 14,
        BorderStyle::ThickThinMediumGap => 15,
        BorderStyle::ThinThickThinMediumGap => 16,
        BorderStyle::ThinThickLargeGap => 17,
        BorderStyle::ThickThinLargeGap => 18,
        BorderStyle::ThinThickThinLargeGap => 19,
        BorderStyle::Wave => 20,
        BorderStyle::DoubleWave => 21,
        BorderStyle::DashSmallGap => 22,
        // dashDotStroked is not assigned a number by MS-OI29500 §2.1.169; place it
        // in the one unused slot (23) so it stays distinct and below threeDEmboss.
        BorderStyle::DashDotStroked => 23,
        BorderStyle::ThreeDEmboss => 24,
        BorderStyle::ThreeDEngrave => 25,
        BorderStyle::Outset => 26,
        BorderStyle::Inset => 27,
        // None, Nil, Dotted, Dashed handled above with early returns
        BorderStyle::None | BorderStyle::Nil | BorderStyle::Dotted | BorderStyle::Dashed => {
            unreachable!()
        }
    };
    size * border_number
}

/// Parse a border color hex string into (R, G, B) components.
/// Returns (0, 0, 0) for None or "auto" (treated as black per Word behavior).
fn parse_border_color_rgb(color: &Option<String>) -> (u32, u32, u32) {
    let s = match color {
        Some(s) if s != "auto" && s.len() == 6 => s.as_str(),
        _ => return (0, 0, 0),
    };
    let r = u32::from_str_radix(&s[0..2], 16).unwrap_or(0);
    let g = u32::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u32::from_str_radix(&s[4..6], 16).unwrap_or(0);
    (r, g, b)
}

/// Compute brightness tiers for color tiebreaker (MS-OI29500 §2.1.169).
/// Returns (primary, secondary, tertiary) where smaller = darker = wins.
fn border_brightness(border: &Border) -> (u32, u32, u32) {
    let (r, g, b) = parse_border_color_rgb(&border.color);
    (r + b + 2 * g, b + 2 * g, g)
}

/// Resolve a border conflict between two borders (MS-OI29500 §2.1.169).
///
/// Resolution cascade:
/// 1. Nil is a suppress directive — if either is nil, no border displays
/// 2. None means "I have no border" — opposing border wins
/// 3. Higher weight wins (weight = size × border_number)
/// 4. Equal weight: darker color wins (brightness = R + B + 2*G, smaller wins)
/// 5. Equal brightness: first argument wins (reading order)
fn resolve_border_conflict(a: &Border, b: &Border) -> Border {
    // MS-OI29500 §2.1.169: nil suppresses everything.
    if a.style == BorderStyle::Nil || b.style == BorderStyle::Nil {
        return if a.style == BorderStyle::Nil {
            a.clone()
        } else {
            b.clone()
        };
    }

    // MS-OI29500 §2.1.169: none yields to opposing border.
    match (a.style == BorderStyle::None, b.style == BorderStyle::None) {
        (true, true) => return a.clone(),
        (true, false) => return b.clone(),
        (false, true) => return a.clone(),
        (false, false) => {}
    }

    // Both are real borders — compare weights.
    let weight_a = border_weight(a);
    let weight_b = border_weight(b);
    if weight_a != weight_b {
        return if weight_b > weight_a {
            b.clone()
        } else {
            a.clone()
        };
    }

    // Equal weight — color brightness tiebreaker (smaller = darker = wins).
    let bright_a = border_brightness(a);
    let bright_b = border_brightness(b);
    if bright_a != bright_b {
        return if bright_a < bright_b {
            a.clone()
        } else {
            b.clone()
        };
    }

    // All equal — first argument wins (reading order).
    a.clone()
}

/// Validate vMerge grid alignment (ISO 29500-1 §17.4.84).
///
/// For each column position, track the grid_span of the most recent vMerge "restart"
/// cell. Any "continue" cell at the same column with a different grid_span is
/// non-conformant -- break its vMerge chain by setting it to VerticalMerge::None.
fn normalize_vmerge_grid_alignment(rows: &mut [TableRowNode]) {
    if rows.is_empty() {
        return;
    }
    let mut active_restart_span: std::collections::HashMap<u32, u32> =
        std::collections::HashMap::new();
    for row in rows.iter_mut() {
        let mut col_pos: u32 = 0;
        for cell in &mut row.cells {
            match cell.v_merge {
                VerticalMerge::Restart => {
                    active_restart_span.insert(col_pos, cell.grid_span);
                }
                VerticalMerge::Continue => match active_restart_span.get(&col_pos) {
                    Some(&restart_span) if restart_span == cell.grid_span => {}
                    _ => {
                        cell.v_merge = VerticalMerge::None;
                        active_restart_span.remove(&col_pos);
                    }
                },
                VerticalMerge::None => {
                    active_restart_span.remove(&col_pos);
                }
            }
            col_pos += cell.grid_span;
        }
    }
}

/// Resolve a single edge: cell border vs table border.
///
/// Per MS-OI29500 §2.1.169:
/// - Nil = "suppress" — no border displayed
/// - None = "I have no border" — opposing (table) border wins
/// - Both visible → weight-based resolution (same algorithm as adjacent cells)
fn resolve_table_vs_cell_border(
    cell_border: &Option<Border>,
    table_border: &Option<Border>,
) -> Option<Border> {
    match (cell_border, table_border) {
        (Some(cell_b), Some(table_b)) => {
            // MS-OI29500: nil suppresses — no border displayed.
            if cell_b.style == BorderStyle::Nil {
                return Some(cell_b.clone());
            }
            // MS-OI29500 §2.1.169: "If the conflicting table cell border is
            // none (no border), then the opposing border shall be displayed."
            if cell_b.style == BorderStyle::None {
                return Some(table_b.clone());
            }
            // Both visible: weight-based resolution (MS-OI29500 §2.1.169).
            Some(resolve_border_conflict(cell_b, table_b))
        }
        (Some(cell_b), None) => Some(cell_b.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}

/// Resolve border conflicts between table-level borders and cell-level borders
/// for all cells in the table (MS-OI29500 §17.4.66(a)).
///
/// After conditional formatting has been applied, this function compares each
/// cell's borders against the table's borders. For each edge, the border with
/// higher weight wins.
fn resolve_table_cell_border_conflicts(
    rows: &mut [TableRowNode],
    table_borders: &Option<BorderSet>,
) {
    let table_borders = match table_borders {
        Some(b) => b,
        None => return,
    };

    let total_rows = rows.len();

    for (row_idx, row) in rows.iter_mut().enumerate() {
        // Compute total grid columns for this row to determine left/right edge positions.
        let total_grid_cols: u32 = row.cells.iter().map(|c| c.grid_span).sum();
        let mut grid_col: u32 = 0;
        let num_cells = row.cells.len();

        for cell_pos in 0..num_cells {
            let cell = &mut row.cells[cell_pos];
            let cell_grid_span = cell.grid_span;

            // ISO 29500-1 §17.4.66: When a cell has no tcBorders, table-level
            // borders serve as defaults. Use an empty BorderSet so the fallback
            // logic below promotes table borders into the cell.
            let cell_borders = cell.formatting.borders.clone().unwrap_or_default();

            // Determine edge positions for correct border source selection.
            let is_top_row = row_idx == 0;
            let is_bottom_row = row_idx == total_rows - 1;
            let is_left_edge = grid_col == 0;
            let is_right_edge = grid_col + cell_grid_span >= total_grid_cols;

            // MS-OI29500 §17.4.66(a): When both cell-level and table-level borders
            // exist on the same edge, the border with the higher weight wins.
            // Exception: an explicit Nil/None cell border is an intentional
            // "suppress" signal and always wins over the table border.

            // Top/bottom: edge rows use table outer borders, interior rows use insideH.
            let table_top = if is_top_row {
                &table_borders.top
            } else {
                &table_borders.inside_h
            };
            let resolved_top = resolve_table_vs_cell_border(&cell_borders.top, table_top);

            let table_bottom = if is_bottom_row {
                &table_borders.bottom
            } else {
                &table_borders.inside_h
            };
            let resolved_bottom = resolve_table_vs_cell_border(&cell_borders.bottom, table_bottom);

            // Left/right: edge cells use table outer borders, interior cells use insideV.
            let table_left = if is_left_edge {
                &table_borders.left
            } else {
                &table_borders.inside_v
            };
            let resolved_left = resolve_table_vs_cell_border(&cell_borders.left, table_left);

            let table_right = if is_right_edge {
                &table_borders.right
            } else {
                &table_borders.inside_v
            };
            let resolved_right = resolve_table_vs_cell_border(&cell_borders.right, table_right);

            cell.formatting.borders = Some(BorderSet {
                top: resolved_top,
                bottom: resolved_bottom,
                left: resolved_left,
                right: resolved_right,
                inside_h: cell_borders.inside_h.clone(),
                inside_v: cell_borders.inside_v.clone(),
            });

            grid_col += cell_grid_span;
        }
    }
}

/// Resolve border conflicts between adjacent cells at shared edges
/// (ISO 29500-1 §17.4.66 rule 3).
///
/// After table-level borders have been cascaded into cells, adjacent cells may
/// specify different borders on a shared edge. The border with the greater
/// weight (per `border_weight()`) wins and replaces the loser on both cells.
///
/// Horizontal shared edges: bottom of row N vs top of row N+1.
/// Vertical shared edges: right of cell at col C vs left of cell at col C+1.
fn resolve_adjacent_cell_border_conflicts(rows: &mut [TableRowNode]) {
    let total_rows = rows.len();

    // --- Vertical shared edges (right of cell C vs left of cell C+1) ---
    for row in rows.iter_mut() {
        let num_cells = row.cells.len();
        if num_cells < 2 {
            continue;
        }
        for cell_pos in 0..num_cells - 1 {
            // Extract the two borders we need to compare.
            let right_border = row.cells[cell_pos]
                .formatting
                .borders
                .as_ref()
                .and_then(|b| b.right.clone());
            let left_border = row.cells[cell_pos + 1]
                .formatting
                .borders
                .as_ref()
                .and_then(|b| b.left.clone());

            let winner = match (&right_border, &left_border) {
                (Some(r), Some(l)) => resolve_border_conflict(r, l),
                (Some(b), None) | (None, Some(b)) => b.clone(),
                (None, None) => continue,
            };

            row.cells[cell_pos]
                .formatting
                .borders
                .get_or_insert_with(BorderSet::default)
                .right = Some(winner.clone());
            row.cells[cell_pos + 1]
                .formatting
                .borders
                .get_or_insert_with(BorderSet::default)
                .left = Some(winner);
        }
    }

    // --- Horizontal shared edges (bottom of row N vs top of row N+1) ---
    for row_idx in 0..total_rows.saturating_sub(1) {
        // Build a grid-column mapping for both rows so we can pair cells
        // that share the same grid column range.
        let top_cells_len = rows[row_idx].cells.len();
        let bot_cells_len = rows[row_idx + 1].cells.len();

        // Build (start_col, end_col) ranges for each cell in both rows.
        let top_ranges: Vec<(u32, u32)> = {
            let mut ranges = Vec::with_capacity(top_cells_len);
            let mut col = 0u32;
            for cell in &rows[row_idx].cells {
                let end = col + cell.grid_span;
                ranges.push((col, end));
                col = end;
            }
            ranges
        };
        let bot_ranges: Vec<(u32, u32)> = {
            let mut ranges = Vec::with_capacity(bot_cells_len);
            let mut col = 0u32;
            for cell in &rows[row_idx + 1].cells {
                let end = col + cell.grid_span;
                ranges.push((col, end));
                col = end;
            }
            ranges
        };

        // For each pair of cells that overlap in grid columns, resolve the
        // bottom/top border conflict. Use a two-pointer approach.
        let mut ti = 0usize;
        let mut bi = 0usize;
        while ti < top_cells_len && bi < bot_cells_len {
            let (t_start, t_end) = top_ranges[ti];
            let (b_start, b_end) = bot_ranges[bi];

            // Check if these cells overlap in grid space.
            if t_start < b_end && b_start < t_end {
                let bottom_border = rows[row_idx].cells[ti]
                    .formatting
                    .borders
                    .as_ref()
                    .and_then(|b| b.bottom.clone());
                let top_border = rows[row_idx + 1].cells[bi]
                    .formatting
                    .borders
                    .as_ref()
                    .and_then(|b| b.top.clone());

                let winner = match (&bottom_border, &top_border) {
                    (Some(bot), Some(top)) => Some(resolve_border_conflict(bot, top)),
                    (Some(b), None) | (None, Some(b)) => Some(b.clone()),
                    (None, None) => None,
                };
                if let Some(w) = winner {
                    rows[row_idx].cells[ti]
                        .formatting
                        .borders
                        .get_or_insert_with(BorderSet::default)
                        .bottom = Some(w.clone());
                    rows[row_idx + 1].cells[bi]
                        .formatting
                        .borders
                        .get_or_insert_with(BorderSet::default)
                        .top = Some(w);
                }
            }

            // Advance the pointer for whichever cell ends first.
            if t_end <= b_end {
                ti += 1;
            }
            if b_end <= t_end {
                bi += 1;
            }
        }
    }
}

/// Apply default cell shading from table style to cells that have no direct shading.
fn apply_default_cell_shading(rows: &mut [TableRowNode], default_cell_shading: &Option<Shading>) {
    if default_cell_shading.is_none() {
        return;
    }
    for row in rows.iter_mut() {
        for cell in row.cells.iter_mut() {
            if cell.formatting.shading.is_none() {
                cell.formatting.shading = default_cell_shading.clone();
            }
        }
    }
}
// =============================================================================
// Table Property Change Parsing (§17.13.5.34/36/37)
// =============================================================================

/// Parse the `w:id` of a `*PrChange` element; 0 when absent/unparseable (the
/// legacy sentinel — never invented, only carried).
fn parse_change_revision_id(change_el: &Element) -> u32 {
    attr_get(change_el, "w:id")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Parse w:tblPrChange from within a w:tblPr element (§17.13.5.34).
/// Returns the previous table formatting before the tracked change.
fn parse_tbl_pr_change(tbl_pr: &Element) -> Result<Option<TableFormattingChange>, RuntimeError> {
    let change_el = match tbl_pr.children.iter().find_map(|child| {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tblPrChange")
        {
            return Some(el);
        }
        None
    }) {
        Some(el) => el,
        None => return Ok(None),
    };

    let author = attr_get(change_el, "w:author").cloned().unwrap_or_default();
    let date = attr_get(change_el, "w:date").cloned();
    let revision_id = parse_change_revision_id(change_el);

    // Inner w:tblPr contains the previous property values
    let inner = tbl_pr_inner_child(change_el, "tblPr");
    let (previous_width, previous_borders, previous_default_cell_margins) = match inner {
        Some(inner_el) => (
            parse_table_measurement(inner_el, "tblW")?,
            parse_border_set(inner_el, "tblBorders")?,
            parse_cell_margins(inner_el, "tblCellMar"),
        ),
        None => (None, None, None),
    };

    Ok(Some(TableFormattingChange {
        previous_width,
        previous_borders,
        previous_default_cell_margins,
        revision_id,
        author,
        date,
    }))
}

/// Parse w:trPrChange from within a w:trPr element (§17.13.5.36).
/// Returns the previous row formatting before the tracked change.
fn parse_tr_pr_change(tr_pr: &Element) -> Result<Option<RowFormattingChange>, RuntimeError> {
    let change_el = match tr_pr.children.iter().find_map(|child| {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "trPrChange")
        {
            return Some(el);
        }
        None
    }) {
        Some(el) => el,
        None => return Ok(None),
    };

    let author = attr_get(change_el, "w:author").cloned().unwrap_or_default();
    let date = attr_get(change_el, "w:date").cloned();
    let revision_id = parse_change_revision_id(change_el);

    // Inner w:trPr contains the previous property values
    let inner = tbl_pr_inner_child(change_el, "trPr");
    let (previous_height, previous_height_rule) = match inner {
        Some(inner_el) => {
            // Look for trHeight inside the inner trPr
            let mut h = None;
            let mut hr = None;
            for child in &inner_el.children {
                if let XMLNode::Element(prop_el) = child
                    && is_w_tag(prop_el, "trHeight")
                {
                    h = attr_get(prop_el, "w:val")
                        .map(|v| parse_twips_measure(v, "trPrChange trHeight element"))
                        .transpose()?;
                    hr = attr_get(prop_el, "w:hRule")
                        .map(|s| HeightRule::from_xml_str(s))
                        .transpose()
                        .map_err(|e| invalid_docx(&format!("trPrChange trHeight: {e}")))?;
                }
            }
            (h, hr)
        }
        None => (None, None),
    };

    Ok(Some(RowFormattingChange {
        revision_id,
        previous_height,
        previous_height_rule,
        author,
        date,
    }))
}

/// Parse w:tcPrChange from within a w:tcPr element (§17.13.5.37).
/// Returns the previous cell formatting before the tracked change.
fn parse_tc_pr_change(tc_pr: &Element) -> Result<Option<CellFormattingChange>, RuntimeError> {
    let change_el = match tc_pr.children.iter().find_map(|child| {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tcPrChange")
        {
            return Some(el);
        }
        None
    }) {
        Some(el) => el,
        None => return Ok(None),
    };

    let author = attr_get(change_el, "w:author").cloned().unwrap_or_default();
    let date = attr_get(change_el, "w:date").cloned();
    let revision_id = parse_change_revision_id(change_el);

    // Inner w:tcPr contains the previous property values
    let inner = tbl_pr_inner_child(change_el, "tcPr");
    let (
        previous_width,
        previous_borders,
        previous_shading,
        previous_v_align,
        previous_margins,
        previous_no_wrap,
        previous_text_direction,
        previous_tc_fit_text,
    ) = match inner {
        Some(inner_el) => {
            let mut v_align = None;
            let mut no_wrap = None;
            let mut text_direction = None;
            let mut tc_fit_text = None;
            for child in &inner_el.children {
                if let XMLNode::Element(el) = child {
                    if is_w_tag(el, "vAlign") {
                        v_align = attr_get(el, "w:val").and_then(|v| match v.as_str() {
                            "top" => Some(VerticalAlignment::Top),
                            "center" => Some(VerticalAlignment::Center),
                            "bottom" => Some(VerticalAlignment::Bottom),
                            _ => None,
                        });
                    }
                    if is_w_tag(el, "noWrap") {
                        no_wrap = Some(!matches!(
                            attr_get(el, "w:val").map(|s| s.as_str()),
                            Some("0") | Some("false") | Some("off")
                        ));
                    }
                    if is_w_tag(el, "textDirection")
                        && let Some(val) = attr_get(el, "w:val")
                    {
                        text_direction = TextDirection::from_xml_str(val).ok();
                    }
                    if is_w_tag(el, "tcFitText") {
                        tc_fit_text = Some(!matches!(
                            attr_get(el, "w:val").map(|s| s.as_str()),
                            Some("0") | Some("false") | Some("off")
                        ));
                    }
                }
            }
            (
                parse_table_measurement(inner_el, "tcW")?,
                parse_border_set(inner_el, "tcBorders")?,
                parse_shading(inner_el)?,
                v_align,
                parse_cell_margins(inner_el, "tcMar"),
                no_wrap,
                text_direction,
                tc_fit_text,
            )
        }
        None => (None, None, None, None, None, None, None, None),
    };

    Ok(Some(CellFormattingChange {
        revision_id,
        previous_width,
        previous_borders,
        previous_shading,
        previous_v_align,
        previous_margins,
        previous_no_wrap,
        previous_text_direction,
        previous_tc_fit_text,
        author,
        date,
    }))
}

/// Find a w: namespaced child element inside a *PrChange element.
fn tbl_pr_inner_child<'a>(change_el: &'a Element, tag: &str) -> Option<&'a Element> {
    change_el.children.iter().find_map(|child| {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, tag)
        {
            return Some(el);
        }
        None
    })
}
/// Compute a structure hash for a table based on its logical grid layout.
///
/// The hash captures:
/// - Number of rows
/// - Number of cells per row
/// - Grid offsets (gridBefore/gridAfter)
/// - Horizontal spans (gridSpan)
/// - Vertical merge patterns (vMerge)
///
/// This allows quick comparison of table structures during diffing.
pub(crate) fn compute_table_structure_hash(rows: &[TableRowNode]) -> String {
    let mut hasher = Sha256::new();

    // Include row count
    hasher.update(rows.len().to_le_bytes());

    for (row_idx, row) in rows.iter().enumerate() {
        // Include row index and cell count
        hasher.update(row_idx.to_le_bytes());
        hasher.update(row.cells.len().to_le_bytes());

        // Include grid offsets
        hasher.update(row.grid_before.to_le_bytes());
        hasher.update(row.grid_after.to_le_bytes());

        for (cell_idx, cell) in row.cells.iter().enumerate() {
            // Include cell index
            hasher.update(cell_idx.to_le_bytes());

            // Include grid span (horizontal merge)
            hasher.update(cell.grid_span.to_le_bytes());

            // Include vertical merge state
            let v_merge_byte: u8 = match cell.v_merge {
                VerticalMerge::None => 0,
                VerticalMerge::Restart => 1,
                VerticalMerge::Continue => 2,
            };
            hasher.update([v_merge_byte]);
        }
    }

    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
/// Extract plain text from inline nodes with Caps mark normalization.
/// Applies `.to_uppercase()` to text nodes with caps=On to match
/// the text projection used by the redline extract path.
pub(crate) fn extract_inline_text_simple(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => {
                if t.style_props.caps == MarkValue::On {
                    out.push_str(&t.text.to_uppercase());
                } else {
                    out.push_str(&t.text);
                }
            }
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(_) => out.push('\u{FFFC}'),
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
        }
    }
    out
}
/// Check if a string is valid legal enumerator content (for parenthesized prefixes).
/// Matches: single letter a-z/A-Z, double letters aa-zz, roman numerals i-xiv, digits 1-999.
pub(crate) fn is_enum_content(s: &str) -> bool {
    // Digits 1-999
    if let Ok(n) = s.parse::<u32>() {
        return (1..=999).contains(&n);
    }
    // Single letter
    if s.len() == 1 {
        return s.as_bytes()[0].is_ascii_alphabetic();
    }
    // Double letters like aa, bb, cc
    if s.len() == 2 {
        let bytes = s.as_bytes();
        if bytes[0] == bytes[1] && bytes[0].is_ascii_lowercase() {
            return true;
        }
    }
    // Roman numerals up to xiv
    matches!(
        s,
        "ii" | "iii" | "iv" | "vi" | "vii" | "viii" | "ix" | "xi" | "xii" | "xiii" | "xiv"
    )
}

/// Try to match a numbering prefix pattern at the start of text.
/// Returns `(label, total_chars_to_strip)` where total includes the trailing separator.
///
/// Supported patterns (must be followed by tab or space):
/// - Parenthesized: (a), (aa), (A), (i)…(xiv), (1), (12)
/// - Digit+period: 1., 12.
/// - Digit+paren: 1), 12)
/// - Letter+period: a., A.
/// - Letter+paren: a), A)
pub(crate) fn match_prefix_pattern(text: &str) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    // Parenthesized: (content) + separator
    if bytes[0] == b'(' {
        if let Some(close) = text[1..].find(')') {
            let inner = &text[1..1 + close];
            if is_enum_content(inner) {
                let label_end = 1 + close + 1; // past ')'
                let label = &text[..label_end];
                // Must have separator (tab or space) after ')'
                if label_end < bytes.len()
                    && (bytes[label_end] == b'\t' || bytes[label_end] == b' ')
                {
                    return Some((label.to_string(), label_end + 1));
                }
            }
        }
        return None;
    }

    // Digit-based: 1. / 1) / 12. / 12) / 4.3 / 10.2.3 / 4.3.
    if bytes[0].is_ascii_digit() {
        // Collect first digit group (up to 3 digits)
        let mut first_end = 0;
        while first_end < bytes.len() && first_end < 3 && bytes[first_end].is_ascii_digit() {
            first_end += 1;
        }
        if first_end > 0 && first_end < bytes.len() {
            // Validate first digit group is a real number (not 0, 00, etc.)
            let first_num_ok = text[..first_end].parse::<u32>().is_ok_and(|n| n >= 1);

            let suffix = bytes[first_end];
            if suffix == b'.' && first_num_ok {
                // Could be single-level "4." or multi-level "4.3", "4.3.", "10.2.3"
                // Try to scan multi-level decimal: digit(s).digit(s)[.digit(s)]*[.]
                let pos = first_end + 1; // past the first dot
                let mut multi_level_end = pos;
                loop {
                    // Scan next digit group (1-3 digits)
                    let group_start = multi_level_end;
                    while multi_level_end < bytes.len()
                        && multi_level_end - group_start < 3
                        && bytes[multi_level_end].is_ascii_digit()
                    {
                        multi_level_end += 1;
                    }
                    if multi_level_end == group_start {
                        break; // No digits found — not a multi-level continuation
                    }
                    // Check for another dot
                    if multi_level_end < bytes.len() && bytes[multi_level_end] == b'.' {
                        multi_level_end += 1;
                        // If next char is a digit, keep scanning more levels
                        if multi_level_end < bytes.len() && bytes[multi_level_end].is_ascii_digit()
                        {
                            continue;
                        }
                        // Trailing dot without more digits — include it and stop
                        break;
                    }
                    break;
                }

                if multi_level_end > pos {
                    // Multi-level decimal found (e.g., "4.3", "4.3.", "10.2.3")
                    let label = &text[..multi_level_end];
                    if multi_level_end < bytes.len()
                        && (bytes[multi_level_end] == b'\t' || bytes[multi_level_end] == b' ')
                    {
                        return Some((label.to_string(), multi_level_end + 1));
                    }
                }

                // Fall back to single-level: "4."
                let label_end = first_end + 1;
                let label = &text[..label_end];
                if label_end < bytes.len()
                    && (bytes[label_end] == b'\t' || bytes[label_end] == b' ')
                {
                    return Some((label.to_string(), label_end + 1));
                }
            } else if suffix == b')' && first_num_ok {
                let label_end = first_end + 1;
                let label = &text[..label_end];
                if label_end < bytes.len()
                    && (bytes[label_end] == b'\t' || bytes[label_end] == b' ')
                {
                    return Some((label.to_string(), label_end + 1));
                }
            }
        }
        return None;
    }

    // Letter-based: a. / A. / a) / A)
    if bytes[0].is_ascii_alphabetic() && bytes.len() >= 2 {
        let suffix = bytes[1];
        if suffix == b'.' || suffix == b')' {
            let label_end = 2;
            let label = &text[..label_end];
            if label_end < bytes.len() && (bytes[label_end] == b'\t' || bytes[label_end] == b' ') {
                return Some((label.to_string(), label_end + 1));
            }
        }
    }

    // Bullet characters: • (U+2022), ◦ (U+25E6), ▪ (U+25AA)
    // Note: we deliberately exclude dashes (–, —, -) as they can appear in normal
    // text followed by tabs, causing false positives.
    let first_char = text.chars().next()?;
    if matches!(
        first_char,
        '•' | '◦' | '▪' | '▸' | '▹' | '■' | '□' | '○' | '●'
    ) {
        let char_len = first_char.len_utf8();
        if char_len < bytes.len() && (bytes[char_len] == b'\t' || bytes[char_len] == b' ') {
            return Some((first_char.to_string(), char_len + 1));
        }
    }

    None
}

/// Detect and strip a typed numbering prefix from the start of paragraph inlines.
/// Returns the stripped prefix text plus the formatting of the first consumed
/// visible prefix text run, and modifies inlines in-place.
///
/// Only strips if there is body text remaining after the prefix (don't strip if the
/// "prefix" IS the entire content).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StrippedLiteralPrefix {
    pub label: String,
    pub leading_tab_count: u8,
    pub has_leading_tab: bool,
    pub has_trailing_tab: bool,
    /// Verbatim whitespace (spaces and tabs, in source order) that preceded the
    /// label. Significant whitespace per XML 1.0 §2.10 — it must round-trip
    /// byte-for-byte, not be discretized to `leading_tab_count` (which drops the
    /// interleaved spaces). The serializer re-emits this verbatim.
    pub leading_ws: String,
    /// Verbatim whitespace (spaces and tabs, in source order) that separated the
    /// label from the body text. Must round-trip verbatim (the prior model
    /// collapsed it to a single space or one tab, losing the real run-length).
    pub trailing_ws: String,
    pub marks: Vec<Mark>,
    pub style_props: StyleProps,
    pub rpr_authored: RunRprAuthored,
    /// Formatting of the FIRST consumed node (the leading whitespace/tab run)
    /// when it differs from the label formatting above — see
    /// `ParagraphNode::literal_prefix_leading_rpr`.
    pub leading_rpr: Option<crate::domain::PrefixLeadingRpr>,
    /// Formatting of the trailing-separator node when it differs — see
    /// `ParagraphNode::literal_prefix_trailing_rpr`.
    pub trailing_rpr: Option<crate::domain::PrefixLeadingRpr>,
}

pub(crate) fn strip_literal_prefix(inlines: &mut Vec<InlineNode>) -> Option<StrippedLiteralPrefix> {
    strip_literal_prefix_with_tracked_flags(inlines, &[])
}

/// Like [`strip_literal_prefix`], but refuses the hoist when any character it
/// would consume comes from a TRACKED inline. `tracked` is aligned
/// index-for-index with `inlines` (empty = nothing is tracked).
///
/// `literal_prefix` is an untracked presentational field: hoisting tracked
/// text into it silently erases the revision status, so the model's
/// accept/reject could never resolve the label (the "prefix-hoist" defect
/// family — e.g. Word redlines that delete the literal "1." while pPrChange
/// adds numPr). A tracked label stays in the body as a tracked segment;
/// post-projection normalization (`normalize_paragraph_after_projection`)
/// re-hoists it once the revision is resolved.
pub(crate) fn strip_literal_prefix_with_tracked_flags(
    inlines: &mut Vec<InlineNode>,
    tracked: &[bool],
) -> Option<StrippedLiteralPrefix> {
    fn is_likely_clause_body_start(ch: char) -> bool {
        ch.is_ascii_uppercase() || matches!(ch, '[' | '"' | '\'' | '“' | '‘')
    }

    // Collect leading text from text nodes, skipping zero-width decorations.
    // We collect whole text nodes to avoid splitting multi-byte characters.
    //
    // The scan budget bounds how far we look for a label pattern, but it is spent
    // only on content PAST the leading whitespace run: a paragraph can carry
    // arbitrarily long leading indentation whitespace, and counting it against the
    // budget can truncate the collected text exactly at the label, hiding the
    // separator that `match_prefix_pattern` requires. That made detection depend on
    // how the runs happened to be split — a whole-document rebuild that coalesces
    // the leading whitespace into the label's own run pushes the separator past a
    // whitespace-inclusive budget, so the SAME paragraph hoisted on first import
    // but not after a rebuild (a non-idempotent hoist the fixpoint gate catches).
    // Skipping the whitespace makes the budget count only the label region, so
    // detection is independent of run boundaries. `PREFIX_SCAN_CEILING` keeps the
    // total scan bounded (an absurdly long all-whitespace run cannot make this
    // loop scan the whole paragraph); it sits far past any real indentation depth,
    // so a genuine label is always inside it.
    const PREFIX_CONTENT_BUDGET: usize = 25;
    const PREFIX_SCAN_CEILING: usize = 512;
    let mut leading_text = String::new();
    for inline in inlines.iter() {
        if leading_text.len() >= PREFIX_SCAN_CEILING {
            break;
        }
        let leading_ws_len = leading_text
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .count();
        if leading_text.len() - leading_ws_len >= PREFIX_CONTENT_BUDGET {
            break;
        }
        match inline {
            InlineNode::Text(t) => {
                leading_text.push_str(&t.text);
            }
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => continue,
            // Stop at hard break or opaque inline
            _ => break,
        }
    }

    // Skip leading whitespace before the enumeration label
    let leading_ws = leading_text
        .bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count();
    let (label, match_len, explicit_separator) =
        if let Some((label, match_len)) = match_prefix_pattern(&leading_text[leading_ws..]) {
            (label, match_len, true)
        } else if leading_ws > 0 && leading_text[leading_ws..].starts_with('(') {
            let text = &leading_text[leading_ws..];
            let close = text[1..].find(')')?;
            let inner = &text[1..1 + close];
            let label_end = 1 + close + 1;
            let body_starts_immediately = text[label_end..]
                .chars()
                .next()
                .is_some_and(is_likely_clause_body_start);
            if is_enum_content(inner) && body_starts_immediately {
                (text[..label_end].to_string(), label_end, false)
            } else {
                return None;
            }
        } else {
            return None;
        };

    // Also consume any additional whitespace after the separator
    let after_match = leading_ws + match_len;
    let trailing_ws = if explicit_separator {
        leading_text[after_match..]
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .count()
    } else {
        0
    };
    let chars_to_strip = after_match + trailing_ws;

    // Check that stripping won't consume all content
    let total_text_len: usize = inlines
        .iter()
        .map(|n| match n {
            InlineNode::Text(t) => t.text.len(),
            _ => 0,
        })
        .sum();
    if chars_to_strip >= total_text_len {
        return None; // Don't strip if prefix IS the entire content
    }

    // Tracked-text guard (see doc comment): walk the inlines the consume
    // loop below would touch — including a partially-consumed final node —
    // and refuse BEFORE mutating anything if any of them is tracked.
    debug_assert!(
        tracked.is_empty() || tracked.len() == inlines.len(),
        "tracked flags must be 1:1 with inlines ({} flags, {} inlines)",
        tracked.len(),
        inlines.len()
    );
    let mut guard_remaining = chars_to_strip;
    for (i, inline) in inlines.iter().enumerate() {
        if guard_remaining == 0 {
            break;
        }
        match inline {
            InlineNode::Text(t) => {
                if tracked.get(i).copied().unwrap_or(false) {
                    return None;
                }
                guard_remaining -= t.text.len().min(guard_remaining);
            }
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
            _ => break,
        }
    }

    let mut prefix_marks = Vec::new();
    let mut prefix_style_props = StyleProps::default();
    let mut prefix_rpr_authored = RunRprAuthored::default();
    let mut captured_prefix_formatting = false;
    let mut fallback_prefix_formatting: Option<(Vec<Mark>, StyleProps, RunRprAuthored)> = None;

    // Strip chars_to_strip characters from the front of text nodes
    let trailing_start = leading_ws + label.len();
    let mut trailing_node_formatting: Option<(Vec<Mark>, StyleProps, RunRprAuthored)> = None;
    let mut remaining = chars_to_strip;
    let mut i = 0;
    while remaining > 0 && i < inlines.len() {
        match &mut inlines[i] {
            InlineNode::Text(t) => {
                let node_start = chars_to_strip - remaining;
                let consumed_len = t.text.len().min(remaining);
                let consumed_text = &t.text[..consumed_len];
                if fallback_prefix_formatting.is_none() {
                    fallback_prefix_formatting =
                        Some((t.marks.clone(), t.style_props.clone(), t.rpr_authored));
                }
                // First node at/after the separator start: the trailing
                // separator's own formatting (see literal_prefix_trailing_rpr).
                if trailing_node_formatting.is_none() && node_start >= trailing_start {
                    trailing_node_formatting =
                        Some((t.marks.clone(), t.style_props.clone(), t.rpr_authored));
                }
                if !captured_prefix_formatting
                    && consumed_text.bytes().any(|b| b != b' ' && b != b'\t')
                {
                    prefix_marks = t.marks.clone();
                    prefix_style_props = t.style_props.clone();
                    prefix_rpr_authored = t.rpr_authored;
                    captured_prefix_formatting = true;
                }
                if t.text.len() <= remaining {
                    remaining -= t.text.len();
                    // Remove this entire text node
                    inlines.remove(i);
                    // Don't increment i since we removed the element
                } else {
                    // Partial strip
                    t.text = t.text[remaining..].to_string();
                    remaining = 0;
                    i += 1;
                }
            }
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {
                // Skip zero-width nodes
                i += 1;
            }
            _ => break, // Stop at hard break or opaque
        }
    }

    if !captured_prefix_formatting
        && let Some((marks, style_props, rpr_authored)) = fallback_prefix_formatting.clone()
    {
        prefix_marks = marks;
        prefix_style_props = style_props;
        prefix_rpr_authored = rpr_authored;
    }

    let leading_tab_count = leading_text[..leading_ws]
        .bytes()
        .filter(|b| *b == b'\t')
        .count() as u8;
    let has_leading_tab = leading_tab_count > 0;
    let leading_ws_str = leading_text[..leading_ws].to_string();
    // Verbatim separator whitespace, captured from the END OF THE LABEL — NOT
    // from `after_match`. For an explicit separator, `match_len` already absorbed
    // the FIRST separator char (`match_len == label.len() + 1`), so capturing
    // from `after_match` would drop that first space/tab. Capturing from the
    // label's end recovers the full separator verbatim. (For the no-separator
    // parenthetical case `label.len() == match_len`, this yields the empty
    // string, as intended.)
    let label_end_in_leading = leading_ws + label.len();
    let trailing_ws_str = leading_text[label_end_in_leading..chars_to_strip].to_string();
    let has_trailing_tab = trailing_ws_str.contains('\t');
    // The leading whitespace/tab run authored its OWN rPr when it differs from
    // the label's — carry it so the leading run re-emits with its authored
    // formatting instead of wearing the label's (the SAFE-template loss).
    // Only meaningful when leading whitespace exists: with none, the fallback
    // is just the label's first fragment.
    let leading_rpr = if leading_ws_str.is_empty() {
        None
    } else {
        fallback_prefix_formatting.filter(|(m, sp, ra)| {
            *m != prefix_marks || *sp != prefix_style_props || *ra != prefix_rpr_authored
        })
    };
    let trailing_rpr = if trailing_ws_str.is_empty() {
        None
    } else {
        trailing_node_formatting.filter(|(m, sp, ra)| {
            *m != prefix_marks || *sp != prefix_style_props || *ra != prefix_rpr_authored
        })
    };
    Some(StrippedLiteralPrefix {
        label,
        leading_tab_count,
        has_leading_tab,
        has_trailing_tab,
        leading_ws: leading_ws_str,
        trailing_ws: trailing_ws_str,
        leading_rpr: leading_rpr.map(|(marks, style_props, rpr_authored)| {
            crate::domain::PrefixLeadingRpr {
                marks,
                style_props,
                rpr_authored,
            }
        }),
        trailing_rpr: trailing_rpr.map(|(marks, style_props, rpr_authored)| {
            crate::domain::PrefixLeadingRpr {
                marks,
                style_props,
                rpr_authored,
            }
        }),
        marks: prefix_marks,
        style_props: prefix_style_props,
        rpr_authored: prefix_rpr_authored,
    })
}
/// Convert an `AtomTrackingContext` to a `TrackingStatus`.
fn tracking_status_from_atom_ctx(ctx: &AtomTrackingContext) -> TrackingStatus {
    let info = RevisionInfo {
        revision_id: ctx.revision_id,
        author: Some(ctx.author.clone()),
        date: ctx.date.clone(),
        apply_op_id: None,
    };
    if let Some(del) = &ctx.stacked_deletion {
        // The stacked state: word_ir normalized both markup orders to
        // insertion-primary, so `info` is the insertion revision.
        return TrackingStatus::InsertedThenDeleted(Box::new(crate::domain::StackedRevision {
            inserted: info,
            deleted: RevisionInfo {
                revision_id: del.revision_id,
                author: Some(del.author.clone()),
                date: del.date.clone(),
                apply_op_id: None,
            },
        }));
    }
    if ctx.is_insertion {
        TrackingStatus::Inserted(info)
    } else {
        TrackingStatus::Deleted(info)
    }
}

/// Group atoms and their corresponding inlines into `TrackedSegment`s.
///
/// Consecutive inlines whose atoms share the same tracking context are merged
/// into a single segment. Atoms without tracking context produce Normal segments.
///
/// `atoms` and `inlines` must be in 1:1 correspondence (same length).
fn segments_from_tracked_atoms(atoms: &[Atom], inlines: Vec<InlineNode>) -> Vec<TrackedSegment> {
    if inlines.is_empty() {
        return Vec::new();
    }

    debug_assert_eq!(
        atoms.len(),
        inlines.len(),
        "atoms and inlines must be 1:1 after prefix stripping"
    );

    let mut segments: Vec<TrackedSegment> = Vec::new();

    for (atom, inline) in atoms.iter().zip(inlines) {
        let status = match &atom.tracking {
            Some(ctx) => tracking_status_from_atom_ctx(ctx),
            None => TrackingStatus::Normal,
        };

        // Extend the current segment if the status matches, otherwise start a new one.
        if let Some(last) = segments.last_mut()
            && last.status == status
        {
            last.inlines.push(inline);
            continue;
        }
        segments.push(TrackedSegment {
            status,
            inlines: vec![inline],
        });
    }

    segments
}

/// Derive a paragraph's heading level (1–9) the ONE way, shared by the body and
/// story import paths so they cannot drift (the body path computed this; the
/// story path used to hardcode `None`, then duplicated the body's logic — this
/// is the de-duplication). A heading is a heading regardless of which story it
/// lives in. It is the resolved `outlineLvl` (direct or via the style chain,
/// §17.3.1.20; 0-based on the wire → 1-based heading level), else a built-in
/// `Heading1`–`Heading9` style id (matched on the DIRECT style id, by name).
///
/// `effective_style_id` is the style id used for outline resolution (the direct
/// id, or the default paragraph style when unstyled, §17.7.4.17).
fn derive_heading_level_number(
    view: &ParagraphView,
    effective_style_id: Option<&str>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
) -> Option<u8> {
    let outline_lvl = if let Some(sd) = style_defs {
        sd.resolve_effective_outline_lvl(effective_style_id, view.outline_lvl)
    } else {
        view.outline_lvl
    };
    // outlineLvl 9 (§17.3.1.20) is the explicit "body text" marker — NOT a
    // heading — so it must not derive a heading level (it would otherwise map to
    // the nonexistent level 10). Only 0-8 denote heading levels 1-9.
    outline_lvl
        .filter(|&lvl| lvl <= 8)
        .map(|lvl| lvl + 1)
        .or_else(|| {
            view.style_id.as_ref().and_then(|s| {
                s.strip_prefix("Heading")
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|&n| (1..=9).contains(&n))
            })
        })
}

#[allow(clippy::too_many_arguments)]
fn paragraph_from_element(
    paragraph: &Element,
    inline_counter: &mut u32,
    block_id_counter: &mut u32,
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    numbering_state: &mut crate::numbering::NumberingState,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
    rel_lookup: &HashMap<String, String>,
) -> Result<BlockNode, RuntimeError> {
    let para_id = attr_get(paragraph, "w14:paraId").cloned();
    let text_id = attr_get(paragraph, "w14:textId").cloned();
    let block_id = NodeId::from(format!("p_{}", *block_id_counter));
    *block_id_counter += 1;
    let view = ParagraphView::from_paragraph(paragraph, rel_lookup).map_err(map_word_ir_error)?;
    let block_text = view.block_text();

    // ISO 29500-1 §17.7.4.17: unstyled paragraphs implicitly reference the default
    // paragraph style (typically "Normal"). Use it for all style resolution below.
    let effective_style_id: Option<&str> = view
        .style_id
        .as_deref()
        .or_else(|| style_defs.and_then(|sd| sd.default_para_style_id()));

    // Computed here (before any field of `view` is moved out below) via the
    // shared helper, so the body and story paths derive heading level identically.
    let heading_level = derive_heading_level_number(&view, effective_style_id, style_defs);

    let mut inlines = inline_nodes_from_atoms(
        &block_id,
        &view.atoms,
        inline_counter,
        style_defs,
        effective_style_id,
    )?;

    // Capture count before stripping so we can offset into atoms later
    let pre_strip_count = inlines.len();

    // Resolve effective numPr: direct wins, else style-resolved (§17.7.4.14).
    // Resolved here (before the literal-prefix strip) so the strip can see
    // whether the paragraph carries structural numbering.
    // DirectNumPr::Suppressed (numId=0, §17.9.18) blocks style and pStyle binding.
    let mut effective_num_props = if let Some(sd) = style_defs {
        sd.resolve_effective_num_props(view.style_id.as_deref(), &view.num_props)
    } else {
        match &view.num_props {
            crate::word_ir::DirectNumPr::Active(np) => Some(np.clone()),
            _ => None,
        }
    };

    // §17.9.23: pStyle reverse binding — if no numPr from direct or style,
    // check if any numbering level claims this paragraph's style via <w:pStyle>.
    // Only applies when numPr is truly absent — Suppressed (numId=0) blocks this.
    if effective_num_props.is_none()
        && view.num_props == crate::word_ir::DirectNumPr::Absent
        && let (Some(style_id), Some(defs)) = (view.style_id.as_deref(), numbering_defs)
    {
        let pstyle_map = defs.build_pstyle_reverse_map();
        if let Some(&(num_id, ilvl)) = pstyle_map.get(style_id) {
            effective_num_props = Some(crate::word_ir::NumProps { num_id, ilvl });
        }
    }

    // MODEL: a paragraph that carries structural numbering (w:numPr resolving to
    // a real numbering level) has its label RENDERED by Word from the numbering
    // definition. A leading label-shaped run baked into such a paragraph's text
    // (e.g. runs ["10. ", "J. MARTINS"] under a numId — common in
    // converted/generated documents) is therefore NOT the rendered label: it is
    // real body text that Word shows in ADDITION to the structural number. It
    // must not be hoisted into `literal_prefix`. Hoisting it there is doubly
    // wrong: the model gets two labels, and the serializer suppresses
    // `literal_prefix` whenever `numbering.is_some()` (Word regenerates the
    // label), so the baked run's bytes vanish on any whole-document rebuild —
    // and reimporting the stripped output hoists the NEXT token ("J. "), eroding
    // more text on every cycle. So: only hoist a literal prefix when the
    // paragraph will NOT carry structural numbering. The condition mirrors what
    // makes `paragraph.numbering` end up `Some` below — an effective numPr AND a
    // resolvable level; a dangling numId that demotes to plain text (its
    // `synthesize` fails via a missing `get_level`) still hoists, matching the
    // demotion fallback that emits `literal_prefix` as a run.
    let has_structural_numbering = effective_num_props
        .as_ref()
        .zip(numbering_defs)
        .is_some_and(|(np, defs)| defs.get_level(np.num_id, np.ilvl).is_some());

    // Strip typed numbering prefix from inlines (e.g., "(a)\t" at start of
    // paragraph) — unless the label text is tracked (then it must stay in
    // the body as a tracked segment so accept/reject can resolve it), or the
    // paragraph already carries structural numbering (see MODEL note above).
    let tracked_flags: Vec<bool> = view.atoms.iter().map(|a| a.tracking.is_some()).collect();
    let strip_result = if has_structural_numbering {
        None
    } else {
        strip_literal_prefix_with_tracked_flags(&mut inlines, &tracked_flags)
    };
    let (
        literal_prefix,
        literal_prefix_leading_tab_count,
        literal_prefix_has_leading_tab,
        literal_prefix_has_trailing_tab,
        literal_prefix_leading_ws,
        literal_prefix_trailing_ws,
        literal_prefix_marks,
        literal_prefix_style_props,
        literal_prefix_rpr_authored,
        literal_prefix_leading_rpr,
        literal_prefix_trailing_rpr,
    ) = match strip_result {
        Some(prefix) => (
            Some(prefix.label),
            prefix.leading_tab_count,
            prefix.has_leading_tab,
            prefix.has_trailing_tab,
            prefix.leading_ws,
            prefix.trailing_ws,
            prefix.marks,
            prefix.style_props,
            prefix.rpr_authored,
            prefix.leading_rpr.map(Box::new),
            prefix.trailing_rpr.map(Box::new),
        ),
        None => (
            None,
            0,
            false,
            false,
            String::new(),
            String::new(),
            Vec::new(),
            StyleProps::default(),
            RunRprAuthored::default(),
            None,
            None,
        ),
    };

    // Number of inlines removed by prefix stripping — skip those atoms
    let prefix_len = pre_strip_count - inlines.len();

    // Body text after prefix stripping, for rendered_text computation
    let body_text = extract_inline_text_simple(&inlines);

    // Extract style_id from view
    let style_id = view.style_id.clone();

    // Resolve alignment through style chain
    let resolved_alignment = if let Some(sd) = style_defs {
        sd.resolve_effective_alignment(effective_style_id, view.alignment.as_deref())
    } else {
        view.alignment.clone()
    };

    // Look up numbering-level indent (§17.9.22) as a base layer for indentation.
    let numbering_level_indent = effective_num_props
        .as_ref()
        .and_then(|np| numbering_defs?.get_level(np.num_id, np.ilvl))
        .and_then(|level| level.indent.as_ref());

    // Resolve indentation: direct paragraph > numbering level > paragraph style (§17.9.22).
    let resolved_indent = if let Some(sd) = style_defs {
        sd.resolve_effective_indent(
            effective_style_id,
            view.indentation.as_ref(),
            numbering_level_indent,
        )
    } else {
        // No style defs — merge direct with numbering level manually.
        let left = view
            .indentation
            .as_ref()
            .and_then(|d| d.left)
            .or_else(|| numbering_level_indent.and_then(|n| n.left));
        let right = view
            .indentation
            .as_ref()
            .and_then(|d| d.right)
            .or_else(|| numbering_level_indent.and_then(|n| n.right));
        let first_line = view
            .indentation
            .as_ref()
            .and_then(|d| d.effective_first_line_twips)
            .or_else(|| numbering_level_indent.and_then(|n| n.effective_first_line_twips));
        // Character-unit indents come from the direct w:ind (numbering's
        // LevelIndent is twips-only). Preserve them — including an explicit 0,
        // which is a real override — instead of dropping to None.
        let start_chars = view.indentation.as_ref().and_then(|d| d.start_chars);
        let end_chars = view.indentation.as_ref().and_then(|d| d.end_chars);
        let first_line_chars = view.indentation.as_ref().and_then(|d| d.first_line_chars);
        let hanging_chars = view.indentation.as_ref().and_then(|d| d.hanging_chars);
        if left.is_some()
            || right.is_some()
            || first_line.is_some()
            || start_chars.is_some()
            || end_chars.is_some()
            || first_line_chars.is_some()
            || hanging_chars.is_some()
        {
            Some(crate::word_ir::IndentProps {
                left,
                right,
                effective_first_line_twips: first_line,
                start_chars,
                end_chars,
                first_line_chars,
                hanging_chars,
            })
        } else {
            None
        }
    };

    // Resolve spacing through style chain
    let resolved_spacing = if let Some(sd) = style_defs {
        sd.resolve_effective_spacing(effective_style_id, view.spacing.as_ref())
    } else {
        view.spacing.clone()
    };

    // Resolve paragraph borders through style chain
    let resolved_borders = if let Some(sd) = style_defs {
        sd.resolve_effective_borders(effective_style_id, view.borders.as_ref())
    } else {
        view.borders.clone()
    };

    // Resolve contextualSpacing through style chain (§17.3.1.9)
    let contextual_spacing = if let Some(sd) = style_defs {
        sd.resolve_effective_contextual_spacing(effective_style_id, view.contextual_spacing)
    } else {
        view.contextual_spacing
    };

    // Resolve widowControl through style chain (§17.3.1.44)
    let widow_control = if let Some(sd) = style_defs {
        sd.resolve_effective_widow_control(effective_style_id, view.widow_control)
    } else {
        view.widow_control
    };

    // Resolve keepNext through style chain (§17.3.1.15)
    let keep_next = if let Some(sd) = style_defs {
        sd.resolve_effective_keep_next(effective_style_id, view.keep_next)
    } else {
        view.keep_next
    };

    // Resolve keepLines through style chain (§17.3.1.14)
    let keep_lines = if let Some(sd) = style_defs {
        sd.resolve_effective_keep_lines(effective_style_id, view.keep_lines)
    } else {
        view.keep_lines
    };

    // Resolve pageBreakBefore through style chain (§17.3.1.23)
    let page_break_before = if let Some(sd) = style_defs {
        sd.resolve_effective_page_break_before(effective_style_id, view.page_break_before)
    } else {
        view.page_break_before
    }
    .unwrap_or(false);

    // Convert alignment string to Alignment enum.
    // MS-OI29500 2.1.45 §17.3.1.13: Word defaults to Left alignment when no
    // jc element is specified anywhere in the style hierarchy.
    let align = Some(
        resolved_alignment
            .as_ref()
            .map(|a| match a.as_str() {
                "left" | "start" => Ok(Alignment::Left),
                "center" => Ok(Alignment::Center),
                "right" | "end" => Ok(Alignment::Right),
                "both" | "justify" => Ok(Alignment::Justify),
                "distribute" => Ok(Alignment::Distribute),
                "highKashida" => Ok(Alignment::HighKashida),
                "lowKashida" => Ok(Alignment::LowKashida),
                "mediumKashida" => Ok(Alignment::MediumKashida),
                "numTab" => Ok(Alignment::NumTab),
                "thaiDistribute" => Ok(Alignment::ThaiDistribute),
                other => Err(invalid_docx(&format!(
                    "jc: unrecognized alignment value {other:?}"
                ))),
            })
            .transpose()?
            .unwrap_or(Alignment::Left),
    ); // absent → spec default

    // Resolve effective tab stops: merge direct paragraph tabs with style hierarchy.
    let resolved_stops = if let Some(sd) = style_defs {
        sd.resolve_effective_tabs(effective_style_id, view.tab_stops.as_deref())
    } else {
        // No style definitions — use direct tab stops only (drop "clear" entries).
        view.tab_stops
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter(|ts| ts.alignment != crate::domain::TabAlignment::Clear)
            .cloned()
            .collect()
    };

    // Synthesize default tab stops for paragraphs with \t but insufficient explicit stops.
    // For tabbed paragraphs, the effective first-line edge = left + first_line,
    // because the first tab starts from the first-line position.
    let left_indent_twips = resolved_indent.as_ref().and_then(|i| i.left).unwrap_or(0);
    let first_line_twips = resolved_indent
        .as_ref()
        .and_then(|i| i.effective_first_line_twips)
        .unwrap_or(0);
    let effective_edge = left_indent_twips + first_line_twips;
    let body_tab_count = body_text.matches('\t').count();
    let prefix_had_tab = literal_prefix_has_leading_tab || literal_prefix_has_trailing_tab;
    let tab_stops_abs = crate::word_ir::synthesize_default_tab_stops(
        &resolved_stops,
        body_tab_count + usize::from(prefix_had_tab),
        default_tab_stop,
        effective_edge,
    );

    // Determine where the body text actually starts after prefix stripping.
    //
    // When strip_literal_prefix removed a label like "(d)\t", the body text
    // begins at the first tab stop position (page-absolute), not at the
    // paragraph's effective_edge. The prefix tab consumed one tab stop to
    // align the body — that tab stop position IS the body's left margin.
    let body_left = if prefix_had_tab {
        // OOXML §17.3.2.38: tab advances to the NEXT stop strictly past
        // the starting location (effective_edge).
        tab_stops_abs
            .iter()
            .find(|s| s.position > effective_edge)
            .map(|s| s.position)
            .unwrap_or(effective_edge)
    } else {
        effective_edge
    };

    // Compute CSS-ready gap (relative to left_indent_twips, because in CSS
    // margin-left comes from left_indent_twips and we suppress text-indent).
    let leading_tab_gap_twips: Option<i32> = if literal_prefix_has_leading_tab {
        Some(body_left - left_indent_twips)
    } else {
        None
    };
    let consumed_prefix_tab_stop_twips: Option<i32> =
        if prefix_had_tab && resolved_stops.iter().any(|s| s.position == body_left) {
            Some(body_left - left_indent_twips)
        } else {
            None
        };

    // DERIVED view value: effective (resolved + synthesized) stops converted
    // from page-margin-absolute to body-left-relative for the frontend. Stops
    // at or before `body_left` are unreachable on the rendered line and drop
    // out. This never round-trips to DOCX — the serializer re-emits only the
    // AUTHORED `view.tab_stops` verbatim.
    let effective_tab_stops_rel: Vec<_> = tab_stops_abs
        .into_iter()
        .map(|mut s| {
            s.position -= body_left;
            s
        })
        .filter(|s| s.position > 0)
        .collect();
    let tab_stops: Vec<_> = view.tab_stops.clone().unwrap_or_default();

    // Convert resolved indentation to domain Indentation struct.
    //
    // Prefix stripped: first_line = cascade value (first_line_twips), unchanged
    // by prefix stripping.  The frontend renders the prefix inline (::before)
    // and uses text-indent (from first_line) to position the whole first line.
    //
    // Tab absorption (no prefix): when body content contains tabs, absorb
    // firstLine into left so the tab grid starts from the effective first-line
    // edge (avoids double-indent with text-indent).
    let indent = if literal_prefix.is_some() {
        Some(Indentation {
            left: Some(left_indent_twips),
            right: resolved_indent.as_ref().and_then(|i| i.right),
            // Preserve explicit firstLine even when the stripped prefix had
            // leading tabs. The tab geometry is tracked separately; clearing
            // the paragraph indent here makes the canonical model less honest
            // and loses source formatting on accept-all.
            effective_first_line_twips: resolved_indent
                .as_ref()
                .and_then(|i| i.effective_first_line_twips)
                .map(|_| first_line_twips),
            // Propagate char-unit values from resolved indent instead of
            // hardcoding None. first_line_chars/hanging_chars are intentionally
            // cleared because the twip-based firstLine survives import.
            start_chars: resolved_indent.as_ref().and_then(|i| i.start_chars),
            end_chars: resolved_indent.as_ref().and_then(|i| i.end_chars),
            first_line_chars: None,
            hanging_chars: None,
        })
    } else if body_tab_count > 0 && first_line_twips < 0 {
        // Tab absorption only fires for hanging indent (negative firstLine).
        // Positive firstLine + tabs must NOT trigger absorption — that would
        // incorrectly shift continuation lines forward.
        Some(Indentation {
            left: Some(effective_edge),
            right: resolved_indent.as_ref().and_then(|i| i.right),
            effective_first_line_twips: None,
            // Propagate char-unit values from resolved indent.
            start_chars: resolved_indent.as_ref().and_then(|i| i.start_chars),
            end_chars: resolved_indent.as_ref().and_then(|i| i.end_chars),
            first_line_chars: None,
            hanging_chars: None,
        })
    } else {
        resolved_indent.as_ref().map(|i| Indentation {
            left: i.left,
            right: i.right,
            effective_first_line_twips: i.effective_first_line_twips,
            start_chars: i.start_chars,
            end_chars: i.end_chars,
            first_line_chars: i.first_line_chars,
            hanging_chars: i.hanging_chars,
        })
    };

    // Detect heading level from:
    //   1. Resolved outlineLvl (direct or from style chain, §17.3.1.20)
    //   2. Built-in style IDs: Heading1–Heading9
    // Note: Title and Subtitle are NOT headings — they're paragraph styles with
    // distinct visual treatment. They keep their alignment/indent as paragraphs
    // and are styled on the frontend via style_id.
    // Synthesize numbering if this paragraph has numPr and we have definitions
    let (numbering, rendered_text) = match (&effective_num_props, numbering_defs) {
        (Some(num_props), Some(defs)) => {
            // Try to synthesize the number text
            match numbering_state.synthesize(defs, num_props.num_id, num_props.ilvl) {
                Ok(synthesized) => {
                    let is_bullet = defs
                        .get_level(num_props.num_id, num_props.ilvl)
                        .is_some_and(|l| l.num_fmt == crate::numbering::NumFormat::Bullet);
                    let numbering_info = crate::domain::NumberingInfo {
                        num_id: num_props.num_id,
                        ilvl: num_props.ilvl,
                        synthesized_text: synthesized.clone(),
                        is_bullet,
                        restart_numbering: false,
                    };
                    // Use body_text (prefix-free) to avoid prefix duplication.
                    // Use the level's suffix (§17.9.28) instead of hardcoding tab.
                    let rendered = if synthesized.is_empty() {
                        None
                    } else {
                        let sep = defs
                            .get_level(num_props.num_id, num_props.ilvl)
                            .map(|l| l.suffix.separator())
                            .unwrap_or("\t");
                        Some(format!("{synthesized}{sep}{body_text}"))
                    };
                    (Some(numbering_info), rendered)
                }
                Err(err) => {
                    // OBSERVABLE DEGRADATION BOUNDARY: numId/ilvl referencing a
                    // dangling or incomplete numbering definition is malformed
                    // producer output that Word itself tolerates (it just
                    // renders the paragraph without a number). Per invariant
                    // #1 (parse totality) we don't refuse the whole import
                    // over one paragraph's broken numPr — we demote it from
                    // list item to plain paragraph, same as "no numPr at all".
                    // The demotion must stay observable rather than silent.
                    tracing::warn!(
                        block_id = %block_id.0,
                        num_id = num_props.num_id,
                        ilvl = num_props.ilvl,
                        error = %err,
                        "numbering synthesis failed; demoting paragraph to plain text (literal prefix fallback)"
                    );
                    // Fall back to literal prefix for rendered_text so the
                    // canonical text stays consistent with the serializer
                    // (which emits literal_prefix as a run).
                    if let Some(ref lp) = literal_prefix {
                        (None, Some(format!("{lp}\t{body_text}")))
                    } else {
                        (None, None)
                    }
                }
            }
        }
        _ => {
            // No structural numbering — use literal prefix for rendered_text if available
            if let Some(ref lp) = literal_prefix {
                (None, Some(format!("{lp}\t{body_text}")))
            } else {
                (None, None)
            }
        }
    };

    let block_text_hash = Some(sha256_hex(block_text.as_bytes()));

    // Convert resolved spacing to domain type
    let spacing = resolved_spacing.map(|sp| ParagraphSpacing {
        before: sp.before,
        after: sp.after,
        before_lines: sp.before_lines,
        after_lines: sp.after_lines,
        before_autospacing: sp.before_autospacing,
        after_autospacing: sp.after_autospacing,
        line: sp.line,
        line_rule: sp.line_rule.as_deref().and_then(|r| match r {
            "auto" => Some(LineSpacingRule::Auto),
            "exact" => Some(LineSpacingRule::Exact),
            "atLeast" => Some(LineSpacingRule::AtLeast),
            _ => None,
        }),
    });

    // Convert resolved borders to domain type
    let borders = convert_paragraph_borders_from_edges(resolved_borders)?;

    // Convert direct paragraph shading
    let shading_authored = view.paragraph_shading.is_some();
    let direct_shading = match view.paragraph_shading {
        Some((fill, val, color)) => {
            let val = val
                .as_deref()
                .map(ShadingPattern::from_xml_str)
                .transpose()
                .map_err(|e| invalid_docx(&format!("paragraph shading: {e}")))?;
            Some(Shading {
                fill,
                val,
                color,
                extra_attrs: Vec::new(),
            })
        }
        None => None,
    };

    // Resolve shading through style chain (§17.3.1.31)
    let shading = if let Some(sd) = style_defs {
        sd.resolve_effective_para_shading(effective_style_id, direct_shading.as_ref())
    } else {
        direct_shading
    };

    Ok(BlockNode::from(ParagraphNode {
        id: block_id,
        style_id,
        align,
        has_direct_align: view.alignment.is_some(),
        indent,
        has_direct_indent: view.indentation.is_some(),
        authored_indent: authored_indentation(view.indentation.as_ref()),
        spacing,
        has_direct_spacing: view.spacing.is_some(),
        authored_spacing: authored_paragraph_spacing(view.spacing.as_ref()),
        borders,
        keep_next,
        keep_lines,
        page_break_before,
        widow_control,
        contextual_spacing,
        shading,
        has_direct_keep_next: view.keep_next.is_some(),
        has_direct_keep_lines: view.keep_lines.is_some(),
        has_direct_page_break_before: view.page_break_before.is_some(),
        has_direct_widow_control: view.widow_control.is_some(),
        has_direct_contextual_spacing: view.contextual_spacing.is_some(),
        has_direct_shading: shading_authored,
        has_direct_borders: view.borders.is_some(),
        tab_stops,
        effective_tab_stops_rel,
        segments: segments_from_tracked_atoms(&view.atoms[prefix_len..], inlines),
        block_text_hash,
        numbering,
        // Emit a direct w:numPr only when the paragraph's OWN pPr authored it.
        // Numbering resolved from a style (§17.7.4.14) or bound via the
        // abstractNum's pStyle reverse link (§17.9.23) is inherited, not direct,
        // and must not be materialized onto the paragraph's pPr on rebuild.
        has_direct_numbering: matches!(view.num_props, crate::word_ir::DirectNumPr::Active(_)),
        numbering_suppressed: matches!(view.num_props, crate::word_ir::DirectNumPr::Suppressed),
        materialized_numbering: None,
        rendered_text,
        literal_prefix,
        literal_prefix_marks,
        literal_prefix_style_props,
        literal_prefix_rpr_authored,
        literal_prefix_leading_rpr,
        literal_prefix_trailing_rpr,
        literal_prefix_leading_tab_twips: leading_tab_gap_twips,
        literal_prefix_leading_tab_count,
        literal_prefix_leading_ws,
        literal_prefix_trailing_ws,
        literal_prefix_has_trailing_tab,
        literal_prefix_trailing_tab_stop_twips: consumed_prefix_tab_stop_twips,
        outline_lvl: view.outline_lvl,
        heading_level: heading_level.map(HeadingLevel::from_number),
        para_mark_status: view.para_mark_status,
        paragraph_mark_marks: convert_text_marks_to_marks(&view.paragraph_mark_rpr),
        paragraph_mark_style_props: convert_text_marks_to_style_props(&view.paragraph_mark_rpr)?,
        paragraph_mark_rpr_off: convert_text_marks_to_para_mark_off(&view.paragraph_mark_rpr),
        para_split: false,
        section_property_change: view.section_property_change,
        formatting_change: view
            .ppr_change
            .as_ref()
            .map(convert_ppr_change)
            .transpose()?,
        section_properties: view.section_properties,
        mirror_indents: view.mirror_indents,
        auto_space_de: view.auto_space_de,
        auto_space_dn: view.auto_space_dn,
        bidi: view.bidi,
        text_alignment: view.text_alignment.clone(),
        suppress_auto_hyphens: view.suppress_auto_hyphens,
        snap_to_grid: view.snap_to_grid,
        overflow_punct: view.overflow_punct,
        adjust_right_ind: view.adjust_right_ind,
        word_wrap: view.word_wrap,
        frame_pr: view
            .frame_pr
            .as_ref()
            .map(|fp| crate::domain::FrameProperties {
                width: fp.width,
                height: fp.height,
                h_rule: fp.h_rule.clone(),
                h_space: fp.h_space,
                v_space: fp.v_space,
                wrap: fp.wrap.clone(),
                v_anchor: fp.v_anchor.clone(),
                h_anchor: fp.h_anchor.clone(),
                x: fp.x,
                x_align: fp.x_align.clone(),
                y: fp.y,
                y_align: fp.y_align.clone(),
                extra_attrs: fp.extra_attrs.clone(),
            }),
        para_id,
        text_id,
        text_direction: view.text_direction.clone(),
        cnf_style: view.cnf_style.clone(),
        preserved_ppr: view.preserved.clone(),
    }))
}
/// Convert a paragraph's OWN direct `w:ind` (the raw pre-cascade parse) into the
/// domain `Indentation` the serializer re-emits verbatim. Unlike the resolved
/// `indent`, this carries ONLY the attributes the direct element authored:
/// inherited numbering/style values are never materialized into it, and the
/// import-time rendering transforms (tab absorption, literal-prefix folding)
/// never touch it. This is the paragraph analogue of the authored `tab_stops`
/// vs the derived `effective_tab_stops_rel`.
fn authored_indentation(
    direct: Option<&crate::word_ir::IndentProps>,
) -> Option<crate::domain::Indentation> {
    direct.map(|i| crate::domain::Indentation {
        left: i.left,
        right: i.right,
        effective_first_line_twips: i.effective_first_line_twips,
        start_chars: i.start_chars,
        end_chars: i.end_chars,
        first_line_chars: i.first_line_chars,
        hanging_chars: i.hanging_chars,
    })
}

/// Convert a paragraph's OWN direct `w:spacing` (the raw pre-cascade parse) into
/// the domain `ParagraphSpacing` the serializer re-emits verbatim. Same
/// authored-vs-effective contract as [`authored_indentation`]: an inherited
/// `after`/`line` is never baked in here. `line_rule` is carried as authored —
/// the "line present ⇒ default lineRule=auto" normalization is a resolution
/// rule, not an authored attribute, so it is NOT applied here.
fn authored_paragraph_spacing(
    direct: Option<&crate::word_ir::SpacingProps>,
) -> Option<crate::domain::ParagraphSpacing> {
    direct.map(|sp| crate::domain::ParagraphSpacing {
        before: sp.before,
        after: sp.after,
        before_lines: sp.before_lines,
        after_lines: sp.after_lines,
        before_autospacing: sp.before_autospacing,
        after_autospacing: sp.after_autospacing,
        line: sp.line,
        line_rule: sp.line_rule.as_deref().and_then(|r| match r {
            "auto" => Some(crate::domain::LineSpacingRule::Auto),
            "exact" => Some(crate::domain::LineSpacingRule::Exact),
            "atLeast" => Some(crate::domain::LineSpacingRule::AtLeast),
            _ => None,
        }),
    })
}

/// Record, per rPr slot, whether the run AUTHORED that property directly.
///
/// `marks` here is the run's OWN `w:rPr` (BEFORE the style cascade is resolved
/// into `style_props`): a slot is authored iff the run carried it. The serializer
/// emits ONLY authored slots, so a value the run merely inherited (resolved into
/// `style_props`) is never re-emitted as direct rPr — which would otherwise inject
/// theme fonts / themeColor that WIN per §17.3.2.26 and change rendering.
///
/// `font_family` (literal ascii/hAnsi) and `font_family_theme` (asciiTheme/
/// hAnsiTheme) are tracked SEPARATELY; likewise `color` (literal/auto) vs
/// `color_theme` (themeColor). This is the fix for the conflation that injected a
/// theme attribute onto a run that authored a literal font (and vice versa).
fn run_rpr_authored(marks: &TextMarks) -> crate::domain::RunRprAuthored {
    crate::domain::RunRprAuthored {
        font_family: marks.font_family.is_some(),
        font_family_theme: marks.font_family_theme.is_some(),
        font_east_asia: marks.font_east_asia.is_some(),
        font_east_asia_theme: marks.font_east_asia_theme.is_some(),
        font_cs: marks.font_cs.is_some(),
        font_cs_theme: marks.font_cs_theme.is_some(),
        font_hint: marks.font_hint.is_some(),
        font_size: marks.font_size.is_some(),
        font_size_cs: marks.font_size_cs.is_some(),
        color: marks.color.is_some(),
        color_theme: marks.color_theme.is_some(),
        lang: marks.lang.is_some(),
        lang_east_asia: marks.lang_east_asia.is_some(),
        kern: marks.kern.is_some(),
        char_spacing: marks.char_spacing.is_some(),
        bold: marks.bold != crate::word_ir::MarkValue::Inherit,
        italic: marks.italic != crate::word_ir::MarkValue::Inherit,
        bold_off: marks.bold == crate::word_ir::MarkValue::Off,
        italic_off: marks.italic == crate::word_ir::MarkValue::Off,
        underline: marks.underline != crate::word_ir::MarkValue::Inherit,
        // `<w:u w:val="none"/>` on the run's own rPr — an explicit OFF override.
        underline_off: marks.underline == crate::word_ir::MarkValue::Off,
        vert_align: marks.subscript != crate::word_ir::MarkValue::Inherit
            || marks.superscript != crate::word_ir::MarkValue::Inherit,
        strike: marks.strike != crate::word_ir::MarkValue::Inherit,
        double_strike: marks.double_strike != crate::word_ir::MarkValue::Inherit,
        caps: marks.caps != crate::word_ir::MarkValue::Inherit,
        small_caps: marks.small_caps != crate::word_ir::MarkValue::Inherit,
        vanish: marks.vanish != crate::word_ir::MarkValue::Inherit,
        web_hidden: marks.web_hidden != crate::word_ir::MarkValue::Inherit,
        emboss: marks.emboss != crate::word_ir::MarkValue::Inherit,
        imprint: marks.imprint != crate::word_ir::MarkValue::Inherit,
        outline: marks.outline != crate::word_ir::MarkValue::Inherit,
        shadow: marks.shadow != crate::word_ir::MarkValue::Inherit,
        bold_cs: marks.bold_cs != crate::word_ir::MarkValue::Inherit,
        italic_cs: marks.italic_cs != crate::word_ir::MarkValue::Inherit,
        rtl: marks.rtl != crate::word_ir::MarkValue::Inherit,
        cs: marks.cs != crate::word_ir::MarkValue::Inherit,
        no_proof: marks.no_proof != crate::word_ir::MarkValue::Inherit,
        spec_vanish: marks.spec_vanish != crate::word_ir::MarkValue::Inherit,
        o_math: marks.o_math != crate::word_ir::MarkValue::Inherit,
        snap_to_grid: marks.snap_to_grid != crate::word_ir::MarkValue::Inherit,
        highlight: marks.highlight.is_some(),
        underline_style: marks.underline_style.is_some(),
        position: marks.position.is_some(),
        char_width_scaling: marks.char_width_scaling.is_some(),
        char_style_id: marks.char_style_id.is_some(),
        run_border: marks.run_border_style.is_some(),
        run_shading: marks.run_shading.is_some(),
        emphasis_mark: marks.emphasis_mark.is_some(),
        text_effect: marks.text_effect.is_some(),
        fit_text: marks.fit_text_width.is_some(),
    }
}

/// Convert TextMarks to a list of Mark variants.
/// Only includes marks that are explicitly On.
fn convert_text_marks_to_marks(text_marks: &TextMarks) -> Vec<Mark> {
    let mut marks = Vec::new();
    if text_marks.bold == WordMarkValue::On {
        marks.push(Mark::Bold);
    }
    if text_marks.italic == WordMarkValue::On {
        marks.push(Mark::Italic);
    }
    if text_marks.underline == WordMarkValue::On {
        marks.push(Mark::Underline);
    }
    if text_marks.subscript == WordMarkValue::On {
        marks.push(Mark::Subscript);
    }
    if text_marks.superscript == WordMarkValue::On {
        marks.push(Mark::Superscript);
    }
    marks
}

/// Extract the paragraph mark's authored OFF toggles (`<w:b w:val="0"/>`,
/// `<w:i w:val="0"/>`, `<w:u w:val="none"/>`) that the presence-only
/// `paragraph_mark_marks: Vec<Mark>` cannot carry. `convert_text_marks_to_marks`
/// keeps only the ON forms, so without this the explicit OFF on a pilcrow would
/// drop silently on every rebuild (see [`crate::domain::ParaMarkRprOff`]).
fn convert_text_marks_to_para_mark_off(text_marks: &TextMarks) -> crate::domain::ParaMarkRprOff {
    crate::domain::ParaMarkRprOff {
        bold_off: text_marks.bold == WordMarkValue::Off,
        italic_off: text_marks.italic == WordMarkValue::Off,
        underline_off: text_marks.underline == WordMarkValue::Off,
    }
}

/// Convert word_ir::MarkValue to domain::MarkValue.
fn convert_mark_value(mv: &crate::word_ir::MarkValue) -> crate::domain::MarkValue {
    match mv {
        crate::word_ir::MarkValue::On => crate::domain::MarkValue::On,
        crate::word_ir::MarkValue::Off => crate::domain::MarkValue::Off,
        crate::word_ir::MarkValue::Inherit => crate::domain::MarkValue::Inherit,
    }
}

/// Extract value-carrying style properties from TextMarks.
fn convert_text_marks_to_style_props(text_marks: &TextMarks) -> Result<StyleProps, RuntimeError> {
    // Build RunBorder from individual fields if a border style is present.
    let run_border = text_marks
        .run_border_style
        .as_ref()
        .map(|style| crate::domain::RunBorder {
            style: style.to_string(),
            size: text_marks.run_border_size.unwrap_or(0),
            space: text_marks.run_border_space.unwrap_or(0),
            color: text_marks
                .run_border_color
                .as_deref()
                .unwrap_or_default()
                .to_string(),
        });
    let highlight = text_marks
        .highlight
        .as_deref()
        .map(HighlightColor::from_xml_str)
        .transpose()
        .map_err(|e| invalid_docx(&format!("run highlight: {e}")))?;
    let underline_style = text_marks
        .underline_style
        .as_deref()
        .map(UnderlineStyle::from_xml_str)
        .transpose()
        .map_err(|e| invalid_docx(&format!("run underline: {e}")))?;
    Ok(StyleProps {
        font_family: text_marks.font_family.clone(),
        font_family_theme: text_marks.font_family_theme.clone(),
        font_size: text_marks.font_size,
        color: text_marks.color.clone(),
        color_theme: text_marks.color_theme.clone(),
        highlight,
        underline_style,
        font_east_asia: text_marks.font_east_asia.clone(),
        font_east_asia_theme: text_marks.font_east_asia_theme.clone(),
        font_cs: text_marks.font_cs.clone(),
        font_cs_theme: text_marks.font_cs_theme.clone(),
        lang: text_marks.lang.clone(),
        lang_east_asia: text_marks.lang_east_asia.clone(),
        char_spacing: text_marks.char_spacing,
        char_style_id: text_marks.char_style_id.clone(),
        run_border,
        position: text_marks.position,
        kern: text_marks.kern,
        char_width_scaling: text_marks.char_width_scaling,
        bold_cs: convert_mark_value(&text_marks.bold_cs),
        italic_cs: convert_mark_value(&text_marks.italic_cs),
        strike: convert_mark_value(&text_marks.strike),
        double_strike: convert_mark_value(&text_marks.double_strike),
        caps: convert_mark_value(&text_marks.caps),
        small_caps: convert_mark_value(&text_marks.small_caps),
        vanish: convert_mark_value(&text_marks.vanish),
        web_hidden: convert_mark_value(&text_marks.web_hidden),
        emboss: convert_mark_value(&text_marks.emboss),
        imprint: convert_mark_value(&text_marks.imprint),
        outline: convert_mark_value(&text_marks.outline),
        shadow: convert_mark_value(&text_marks.shadow),
        font_size_cs: text_marks.font_size_cs,
        rtl: convert_mark_value(&text_marks.rtl),
        cs: convert_mark_value(&text_marks.cs),
        font_hint: text_marks.font_hint.clone(),
        no_proof: convert_mark_value(&text_marks.no_proof),
        spec_vanish: convert_mark_value(&text_marks.spec_vanish),
        o_math: convert_mark_value(&text_marks.o_math),
        snap_to_grid: convert_mark_value(&text_marks.snap_to_grid),
        run_shading: {
            match &text_marks.run_shading {
                Some((fill, val, color)) => {
                    let val = val
                        .as_deref()
                        .map(ShadingPattern::from_xml_str)
                        .transpose()
                        .map_err(|e| invalid_docx(&format!("run shading: {e}")))?;
                    Some(Shading {
                        fill: fill.clone(),
                        val,
                        color: color.clone(),
                        extra_attrs: Vec::new(),
                    })
                }
                None => None,
            }
        },
        emphasis_mark: text_marks
            .emphasis_mark
            .as_deref()
            .map(EmphasisMark::from_xml_str)
            .transpose()
            .map_err(|e| invalid_docx(&format!("run emphasis mark: {e}")))?,
        text_effect: text_marks
            .text_effect
            .as_deref()
            .map(TextEffect::from_xml_str)
            .transpose()
            .map_err(|e| invalid_docx(&format!("run text effect: {e}")))?,
        fit_text: {
            text_marks.fit_text_width.map(|width| FitText {
                width,
                id: text_marks.fit_text_id,
            })
        },
        preserved: text_marks.preserved.clone(),
    })
}

/// Re-run the run-property style cascade for a single run against a (possibly
/// changed) paragraph style, updating its resolved `marks` / `style_props` in
/// place.
///
/// At import a run's `marks` / `style_props` hold the values RESOLVED through
/// the style cascade (`StyleDefinitions::resolve`) in force at that time —
/// style-inherited marks (caps, bold, fonts, …) are baked in. When accept/reject
/// reverts or applies a tracked paragraph-style change (`w:pPrChange`,
/// ECMA-376 §17.13.5.29) the paragraph's `style_id` changes but the runs keep
/// their old resolved values, so e.g. a caps-bearing style that is rejected
/// leaves `caps == On` baked on the run and the text still renders uppercase.
///
/// The fix re-runs the SAME cascade against the resulting style. To do so it
/// reconstructs the run's DIRECT `TextMarks` — the serializer already knows how
/// to project a run down to its authored-only rPr (`build_run_direct_rpr`), so
/// we build that element and parse it back (`parse_rpr_element`) rather than
/// hand-rolling a parallel domain→word_ir inverse (whose per-mark whitelist
/// could silently drift from the import path). `resolve` then reproduces exactly
/// what a fresh import against `para_style_id` would yield.
pub(crate) fn reresolve_run_style_props(
    marks: &mut Vec<Mark>,
    style_props: &mut StyleProps,
    rpr_authored: crate::domain::RunRprAuthored,
    style_defs: &crate::styles::StyleDefinitions,
    para_style_id: Option<&str>,
) -> Result<(), RuntimeError> {
    let direct_rpr = crate::serialize::build_run_direct_rpr(marks, style_props, rpr_authored);
    let direct = crate::word_ir::parse_rpr_element(&direct_rpr);
    // Mirror import's char-style substitution: an unset rStyle resolves through
    // the default character style (§17.7.4.17) if one exists.
    let char_style_id = direct
        .char_style_id
        .as_deref()
        .or_else(|| style_defs.default_char_style_id());
    let resolved = style_defs.resolve(&direct, char_style_id, para_style_id);
    *marks = convert_text_marks_to_marks(&resolved);
    *style_props = convert_text_marks_to_style_props(&resolved)?;
    Ok(())
}

/// Convert a word_ir::PprChange to a domain::ParagraphFormattingChange.
///
/// The snapshot's values are converted RAW (no style-chain resolution): the
/// inner pPr is the previous DIRECT formatting per §17.13.5.29, and the
/// serializer re-emits it as-is.
fn convert_ppr_change(
    ppr_change: &crate::word_ir::PprChange,
) -> Result<ParagraphFormattingChange, RuntimeError> {
    let previous_alignment =
        ppr_change
            .previous_alignment
            .as_ref()
            .and_then(|a| match a.as_str() {
                "left" | "start" => Some(Alignment::Left),
                "center" => Some(Alignment::Center),
                "right" | "end" => Some(Alignment::Right),
                "both" | "justify" => Some(Alignment::Justify),
                "distribute" => Some(Alignment::Distribute),
                "highKashida" => Some(Alignment::HighKashida),
                "lowKashida" => Some(Alignment::LowKashida),
                "mediumKashida" => Some(Alignment::MediumKashida),
                "numTab" => Some(Alignment::NumTab),
                "thaiDistribute" => Some(Alignment::ThaiDistribute),
                _ => None,
            });
    let previous_indentation = ppr_change
        .previous_indentation
        .as_ref()
        .map(|i| Indentation {
            left: i.left,
            right: i.right,
            effective_first_line_twips: i.effective_first_line_twips,
            start_chars: i.start_chars,
            end_chars: i.end_chars,
            first_line_chars: i.first_line_chars,
            hanging_chars: i.hanging_chars,
        });
    let previous_spacing = ppr_change
        .previous_spacing
        .as_ref()
        .map(|sp| ParagraphSpacing {
            before: sp.before,
            after: sp.after,
            before_lines: sp.before_lines,
            after_lines: sp.after_lines,
            before_autospacing: sp.before_autospacing,
            after_autospacing: sp.after_autospacing,
            line: sp.line,
            line_rule: sp.line_rule.as_deref().and_then(|r| match r {
                "auto" => Some(LineSpacingRule::Auto),
                "exact" => Some(LineSpacingRule::Exact),
                "atLeast" => Some(LineSpacingRule::AtLeast),
                _ => None,
            }),
        });
    let previous_shading = match &ppr_change.previous_shading {
        Some((fill, val, color)) => {
            let val = val
                .as_deref()
                .map(ShadingPattern::from_xml_str)
                .transpose()
                .map_err(|e| invalid_docx(&format!("pPrChange previous shading: {e}")))?;
            Some(Shading {
                fill: fill.clone(),
                val,
                color: color.clone(),
                extra_attrs: Vec::new(),
            })
        }
        None => None,
    };
    Ok(ParagraphFormattingChange {
        previous_alignment,
        previous_indentation,
        previous_spacing,
        // `previous_numbering` needs `NumberingInfo` (synthesized text +
        // bullet flag), which requires the document-order numbering counter
        // state — not available here. The snapshot's numPr instead stays in
        // `previous_preserved_ppr` and round-trips/restores verbatim (see
        // `PPR_CHANGE_MODELED_CHILDREN`).
        previous_numbering: None,
        previous_numbering_explicitly_absent: false,
        previous_style_id: ppr_change.previous_style_id.clone(),
        previous_keep_next: ppr_change.previous_keep_next,
        previous_keep_lines: ppr_change.previous_keep_lines,
        // Domain models this as bool (same collapse as the outer paragraph):
        // an explicit `w:pageBreakBefore w:val="0"` restores as absent.
        previous_page_break_before: ppr_change.previous_page_break_before == Some(true),
        previous_widow_control: ppr_change.previous_widow_control,
        previous_contextual_spacing: ppr_change.previous_contextual_spacing,
        previous_shading,
        previous_borders: convert_paragraph_borders_from_edges(ppr_change.previous_borders.clone())
            .map_err(|e| invalid_docx(&format!("pPrChange previous borders: {}", e.message)))?,
        previous_tab_stops: ppr_change.previous_tab_stops.clone().unwrap_or_default(),
        // Internal literal-prefix synthesis state — never present in Word XML.
        previous_literal_prefix_leading_tab_twips: None,
        previous_literal_prefix_trailing_tab_stop_twips: None,
        previous_paragraph_mark_marks: convert_text_marks_to_marks(
            &ppr_change.previous_paragraph_mark_rpr,
        ),
        previous_paragraph_mark_style_props: convert_text_marks_to_style_props(
            &ppr_change.previous_paragraph_mark_rpr,
        )
        .expect("paragraph mark rPr in pPrChange should convert"),
        previous_paragraph_mark_rpr_off: convert_text_marks_to_para_mark_off(
            &ppr_change.previous_paragraph_mark_rpr,
        ),
        previous_text_direction: ppr_change.previous_text_direction.clone(),
        previous_text_alignment: ppr_change.previous_text_alignment.clone(),
        previous_mirror_indents: ppr_change.previous_mirror_indents,
        previous_auto_space_de: ppr_change.previous_auto_space_de,
        previous_auto_space_dn: ppr_change.previous_auto_space_dn,
        previous_bidi: ppr_change.previous_bidi,
        previous_suppress_auto_hyphens: ppr_change.previous_suppress_auto_hyphens,
        previous_snap_to_grid: ppr_change.previous_snap_to_grid,
        previous_overflow_punct: ppr_change.previous_overflow_punct,
        previous_adjust_right_ind: ppr_change.previous_adjust_right_ind,
        previous_word_wrap: ppr_change.previous_word_wrap,
        previous_frame_pr: ppr_change.previous_frame_pr.as_ref().map(|fp| {
            crate::domain::FrameProperties {
                width: fp.width,
                height: fp.height,
                h_rule: fp.h_rule.clone(),
                h_space: fp.h_space,
                v_space: fp.v_space,
                wrap: fp.wrap.clone(),
                v_anchor: fp.v_anchor.clone(),
                h_anchor: fp.h_anchor.clone(),
                x: fp.x,
                x_align: fp.x_align.clone(),
                y: fp.y,
                y_align: fp.y_align.clone(),
                extra_attrs: fp.extra_attrs.clone(),
            }
        }),
        previous_preserved_ppr: ppr_change.preserved.clone(),
        revision_id: ppr_change.revision_id,
        author: ppr_change.author.clone(),
        date: ppr_change.date.clone(),
    })
}

/// Convert a word_ir::RprChange to a domain::FormattingChange.
fn convert_rpr_change(text_marks: &TextMarks) -> Result<Option<FormattingChange>, RuntimeError> {
    let rpr = match text_marks.rpr_change.as_ref() {
        Some(rpr) => rpr,
        None => return Ok(None),
    };
    Ok(Some(FormattingChange {
        previous_marks: convert_text_marks_to_marks(&rpr.previous_marks),
        previous_style_props: convert_text_marks_to_style_props(&rpr.previous_marks)?,
        // Same derivation `run_rpr_authored` already applies to a run's LIVE
        // rPr — the embedded previous-state rPr inside a real w:rPrChange
        // (Word- or engine-authored) is itself parsed into a TextMarks, so
        // its authored-bitset is derivable the same way, not just defaulted.
        previous_rpr_authored: run_rpr_authored(&rpr.previous_marks),
        revision_id: rpr.revision_id,
        author: rpr.author.clone(),
        date: rpr.date.clone(),
    }))
}
fn inline_nodes_from_atoms(
    block_id: &NodeId,
    atoms: &[Atom],
    inline_counter: &mut u32,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    para_style_id: Option<&str>,
) -> Result<Vec<InlineNode>, RuntimeError> {
    let mut out = Vec::new();
    for atom in atoms {
        // Resolve marks through style inheritance chain if style definitions are available.
        // Per ISO 29500-1 §17.7.4.17, when a run has no explicit rStyle, use the
        // default character style (w:type="character" w:default="1") if one exists.
        let resolved_marks = match style_defs {
            Some(sd) => {
                let char_style_id = atom
                    .marks
                    .char_style_id
                    .as_deref()
                    .or_else(|| sd.default_char_style_id());
                sd.resolve(&atom.marks, char_style_id, para_style_id)
            }
            None => atom.marks.clone(),
        };
        match &atom.kind {
            AtomKind::Text(text) => {
                if text.is_empty() {
                    continue;
                }
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_t{}", block_id.0, local_index));
                out.push(InlineNode::from(TextNode {
                    id,
                    text_role: None,
                    text: text.clone(),
                    marks: convert_text_marks_to_marks(&resolved_marks),
                    style_props: convert_text_marks_to_style_props(&resolved_marks)?,
                    rpr_authored: run_rpr_authored(&atom.marks),
                    formatting_change: convert_rpr_change(&atom.marks)?,
                }));
            }
            AtomKind::Tab => {
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_t{}", block_id.0, local_index));
                out.push(InlineNode::from(TextNode {
                    id,
                    text_role: None,
                    text: "\t".to_string(),
                    marks: convert_text_marks_to_marks(&resolved_marks),
                    style_props: convert_text_marks_to_style_props(&resolved_marks)?,
                    rpr_authored: run_rpr_authored(&atom.marks),
                    formatting_change: convert_rpr_change(&atom.marks)?,
                }));
            }
            // §17.3.3.18: a non-breaking hyphen is a visible character. On the
            // rebuild path it becomes a literal U+2011 in text (Word reads U+2011
            // identically to <w:noBreakHyphen/>); untouched runs round-trip the
            // element verbatim. Mirrors the Tab arm above.
            AtomKind::NoBreakHyphen => {
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_t{}", block_id.0, local_index));
                out.push(InlineNode::from(TextNode {
                    id,
                    text_role: None,
                    text: "\u{2011}".to_string(),
                    marks: convert_text_marks_to_marks(&resolved_marks),
                    style_props: convert_text_marks_to_style_props(&resolved_marks)?,
                    rpr_authored: run_rpr_authored(&atom.marks),
                    formatting_change: convert_rpr_change(&atom.marks)?,
                }));
            }
            AtomKind::Break(break_type) => {
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_br{}", block_id.0, local_index));
                out.push(InlineNode::HardBreak(crate::domain::HardBreakNode {
                    id,
                    break_type: break_type.clone(),
                }));
            }
            AtomKind::Widget { name, raw_xml } => {
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_widget_{}", block_id.0, local_index));
                let proof_ref = ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: id.clone(),
                    docx_anchor: format!("paragraph:{}:atom:{}", block_id.0, local_index),
                };
                // Classify the opaque kind from the element name
                let local_name = name.split(':').next_back().unwrap_or(name);
                let kind = match local_name {
                    "drawing" | "pict" | "object" => OpaqueKind::Drawing,
                    "oMathPara" => OpaqueKind::OmmlBlock,
                    "oMath" => OpaqueKind::OmmlInline,
                    "fldChar" | "instrText" | "delInstrText" | "fldSimple" => {
                        OpaqueKind::Field(extract_field_data(local_name, raw_xml)?)
                    }
                    "sdt" => OpaqueKind::Sdt,
                    "ruby" => OpaqueKind::Ruby,
                    "commentReference" | "footnoteReference" | "endnoteReference" => {
                        let ref_data = extract_note_reference_data(raw_xml)?;
                        match local_name {
                            "commentReference" => OpaqueKind::CommentReference(ref_data),
                            "footnoteReference" => OpaqueKind::FootnoteReference(ref_data),
                            _ => OpaqueKind::EndnoteReference(ref_data),
                        }
                    }
                    "smartTag" => OpaqueKind::SmartTag,
                    "sym" => OpaqueKind::Sym(extract_sym_data(raw_xml)?),
                    "ptab" => OpaqueKind::Ptab,
                    "customXml" => OpaqueKind::CustomXml,
                    _ => OpaqueKind::Unknown(name.clone()),
                };
                let content_hash = Some(sha256_hex(raw_xml));
                out.push(InlineNode::from(crate::domain::OpaqueInlineNode {
                    id,
                    kind,
                    opaque_ref: format!("paragraph:{}:widget:{}", block_id.0, local_index),
                    proof_ref,
                    wrapper_marks: convert_text_marks_to_marks(&atom.marks),
                    wrapper_style_props: convert_text_marks_to_style_props(&atom.marks)?,
                    raw_xml: Some(raw_xml.clone()),
                    content_hash,
                }));
            }
            AtomKind::Hyperlink(data) => {
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_hyperlink_{}", block_id.0, local_index));
                let proof_ref = ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: id.clone(),
                    docx_anchor: format!("paragraph:{}:hyperlink:{}", block_id.0, local_index),
                };
                out.push(InlineNode::from(crate::domain::OpaqueInlineNode {
                    id,
                    kind: OpaqueKind::Hyperlink(data.clone()),
                    opaque_ref: format!("paragraph:{}:hyperlink:{}", block_id.0, local_index),
                    proof_ref,
                    wrapper_marks: convert_text_marks_to_marks(&atom.marks),
                    wrapper_style_props: convert_text_marks_to_style_props(&atom.marks)?,
                    raw_xml: None, // Hyperlinks use HyperlinkData for serialization
                    content_hash: None,
                }));
            }
            AtomKind::Decoration { name, raw_xml } => {
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_deco_{}", block_id.0, local_index));
                let proof_ref = ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: id.clone(),
                    docx_anchor: format!("paragraph:{}:atom:{}", block_id.0, local_index),
                };
                // Determine decoration type from element name
                let deco_type = decoration_type_from_name(name);
                // For the glyph-rendering note marks (footnoteRef/endnoteRef,
                // separator/continuationSeparator, annotationRef) the host run's
                // rPr is load-bearing: Word draws the auto-number / separator /
                // ref mark in that character formatting, and `raw_xml` holds only
                // the bare marker, so capture the run's properties for the
                // serializer to re-synthesize (else the mark reverts to the
                // default style on every story rebuild). For the other run-level
                // decorations (`lastRenderedPageBreak`, `softHyphen`) the mark
                // renders nothing positioned by an independent rPr; they share a
                // run with adjacent text, so re-emitting that run's rPr onto the
                // atom-split marker run would only duplicate the formatting the
                // text run already carries — leave those empty.
                let (wrapper_marks, wrapper_style_props) =
                    if decoration_wrapper_rpr_is_load_bearing(name) {
                        (
                            convert_text_marks_to_marks(&atom.marks),
                            convert_text_marks_to_style_props(&atom.marks)?,
                        )
                    } else {
                        (Vec::new(), StyleProps::default())
                    };
                out.push(InlineNode::from(DecorationNode {
                    id,
                    kind: deco_type,
                    opaque_ref: format!("paragraph:{}:deco:{}", block_id.0, local_index),
                    proof_ref,
                    wrapper_marks,
                    wrapper_style_props,
                    raw_xml: Some(raw_xml.clone()),
                    origin: None,
                }));
            }
            AtomKind::CommentRangeStart { id } => {
                out.push(InlineNode::CommentRangeStart { id: id.clone() });
            }
            AtomKind::CommentRangeEnd { id } => {
                out.push(InlineNode::CommentRangeEnd { id: id.clone() });
            }
            AtomKind::TrackedMoveStart { raw_xml } | AtomKind::TrackedMoveEnd { raw_xml } => {
                // Both markers carry the (childless) w:moveFrom/w:moveTo wrapper
                // bytes. The serializer re-wraps the move content between them on
                // the import round-trip path, so the raw XML MUST be preserved —
                // a decoration with raw_xml: None cannot be serialized.
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_deco_{}", block_id.0, local_index));
                let proof_ref = ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: id.clone(),
                    docx_anchor: format!("paragraph:{}:atom:{}", block_id.0, local_index),
                };
                out.push(InlineNode::from(DecorationNode {
                    id,
                    kind: DecorationType::MoveRange,
                    opaque_ref: format!("paragraph:{}:deco:{}", block_id.0, local_index),
                    proof_ref,
                    // Zero-width move-range wrapper marker: no host run rPr.
                    wrapper_marks: Vec::new(),
                    wrapper_style_props: StyleProps::default(),
                    raw_xml: Some(raw_xml.clone()),
                    origin: None,
                }));
            }
            AtomKind::BidiWrapperStart { raw_xml } | AtomKind::BidiWrapperEnd { raw_xml } => {
                // Both markers carry the (childless) w:bdo/w:dir wrapper bytes.
                // The display-only wrapper is TRANSPARENT — its inner runs already
                // surfaced as ordinary text atoms before/after these markers — so
                // each marker is a zero-width decoration carrying the wrapper bytes
                // the serializer re-wraps on the round-trip path (mirrors
                // TrackedMove*). raw_xml MUST be preserved; raw_xml: None cannot be
                // serialized.
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_deco_{}", block_id.0, local_index));
                let proof_ref = ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: id.clone(),
                    docx_anchor: format!("paragraph:{}:atom:{}", block_id.0, local_index),
                };
                out.push(InlineNode::from(DecorationNode {
                    id,
                    kind: DecorationType::BidiWrapper,
                    opaque_ref: format!("paragraph:{}:deco:{}", block_id.0, local_index),
                    proof_ref,
                    // Zero-width bidi-wrapper marker: no host run rPr.
                    wrapper_marks: Vec::new(),
                    wrapper_style_props: StyleProps::default(),
                    raw_xml: Some(raw_xml.clone()),
                    origin: None,
                }));
            }
            AtomKind::CustomXmlWrapperStart { raw_xml }
            | AtomKind::CustomXmlWrapperEnd { raw_xml } => {
                // Both markers carry the wrapper bytes with content children
                // cleared but the customXmlPr/smartTagPr property child kept.
                // The customXml/smartTag wrapper is TRANSPARENT — its inner runs
                // already surfaced as ordinary text atoms (and inner revisions
                // as ordinary revision atoms) between these markers — so each
                // marker is a zero-width decoration carrying the wrapper bytes
                // the serializer re-nests on round-trip (mirrors BidiWrapper).
                // raw_xml MUST be preserved; raw_xml: None cannot be serialized.
                let local_index = *inline_counter;
                *inline_counter += 1;
                let id = NodeId::from(format!("{}_deco_{}", block_id.0, local_index));
                let proof_ref = ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: id.clone(),
                    docx_anchor: format!("paragraph:{}:atom:{}", block_id.0, local_index),
                };
                out.push(InlineNode::from(DecorationNode {
                    id,
                    kind: DecorationType::CustomXmlWrapper,
                    opaque_ref: format!("paragraph:{}:deco:{}", block_id.0, local_index),
                    proof_ref,
                    // Zero-width customXml/smartTag wrapper marker: no host run rPr.
                    wrapper_marks: Vec::new(),
                    wrapper_style_props: StyleProps::default(),
                    raw_xml: Some(raw_xml.clone()),
                    origin: None,
                }));
            }
        }
    }
    Ok(out)
}
// =============================================================================
// Story parsing functions
// =============================================================================

/// Parse the main document part's relationships. The relationships part path is
/// derived from the resolved main part name (OPC §9.3.4), not hardcoded to
/// `word/_rels/document.xml.rels`.
pub(crate) fn parse_document_relationships(
    archive: &DocxArchive,
    main_part_name: &str,
) -> Result<DocumentRelationships, RuntimeError> {
    let rels_path = crate::docx_package::rels_part_path(main_part_name);

    let rels_xml = archive.get(&rels_path).ok_or_else(|| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("missing {rels_path}"),
        details: ErrorDetails::default(),
    })?;

    let root = word_xml::parse_document_xml(rels_xml).map_err(|err| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("failed to parse {rels_path}"),
        details: ErrorDetails {
            context: Some(format!("{err:?}")),
            ..ErrorDetails::default()
        },
    })?;

    let mut rels = DocumentRelationships::default();

    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        // Match Relationship element (with or without namespace prefix)
        let local_name = local_element_name(el);
        if local_name != "Relationship" {
            continue;
        }

        let id = attr_get(el, "Id")
            .ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!("{rels_path}: Relationship element missing required Id attribute"),
                details: ErrorDetails::default(),
            })?
            .clone();
        let rel_type = attr_get(el, "Type")
            .ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!("{rels_path}: Relationship {id} missing required Type attribute"),
                details: ErrorDetails::default(),
            })?
            .clone();
        let target = attr_get(el, "Target")
            .ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!(
                    "{rels_path}: Relationship {id} missing required Target attribute"
                ),
                details: ErrorDetails::default(),
            })?
            .clone();

        let rel = Relationship { id, target };

        if rel_type == HEADER_REL_TYPE {
            rels.headers.push(rel);
        } else if rel_type == FOOTER_REL_TYPE {
            rels.footers.push(rel);
        } else if rel_type == FOOTNOTES_REL_TYPE {
            rels.footnotes = Some(rel);
        } else if rel_type == ENDNOTES_REL_TYPE {
            rels.endnotes = Some(rel);
        } else if rel_type == COMMENTS_REL_TYPE {
            rels.comments = Some(rel);
        } else if rel_type == COMMENTS_EXTENDED_REL_TYPE {
            rels.comments_extended = Some(rel);
        } else if rel_type == CUSTOM_XML_REL_TYPE {
            rels.custom_xml.push(rel);
        } else if rel_type == HYPERLINK_REL_TYPE {
            // External hyperlinks: Target is the actual URL
            rels.hyperlinks.insert(rel.id, rel.target);
        }
    }

    Ok(rels)
}
/// Resolve external hyperlink URLs in the canonical document using
/// the relationship map (rId -> URL) parsed from document.xml.rels.
pub(crate) fn resolve_hyperlink_urls(
    doc: &mut CanonDoc,
    hyperlinks: &std::collections::HashMap<String, String>,
) {
    if hyperlinks.is_empty() {
        return;
    }
    for block in &mut doc.blocks {
        resolve_hyperlink_urls_in_block(&mut block.block, hyperlinks);
    }
}

/// Resolve hyperlink rId→URL in a block, recursing into table cells so links
/// inside tables are resolved too (not just top-level paragraphs).
fn resolve_hyperlink_urls_in_block(
    block: &mut BlockNode,
    hyperlinks: &std::collections::HashMap<String, String>,
) {
    match block {
        BlockNode::Paragraph(p) => {
            for segment in &mut p.segments {
                for inline in &mut segment.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && let OpaqueKind::Hyperlink(ref mut data) = o.kind
                        && let Some(ref r_id) = data.r_id
                        && let Some(url) = hyperlinks.get(r_id)
                    {
                        data.url = Some(url.clone());
                    }
                }
            }
        }
        BlockNode::Table(t) => {
            for row in &mut t.rows {
                for cell in &mut row.cells {
                    for cell_block in &mut cell.blocks {
                        resolve_hyperlink_urls_in_block(cell_block, hyperlinks);
                    }
                }
            }
        }
        _ => {}
    }
}
/// Build a lookup from relationship ID (e.g. "rId5") to base64 data URI for images.
///
/// Parses ALL relationship files in `word/_rels/` (document.xml.rels, header*.xml.rels,
/// footer*.xml.rels, footnotes.xml.rels, etc.) to map rId → target path, then reads
/// matching image files from the DOCX ZIP archive and encodes them as data URIs.
///
/// Returns an error if any `.rels` file contains malformed XML or is missing
/// required OPC attributes (Id, Type, Target) — these indicate a corrupt
/// archive, not a recoverable condition.
pub fn build_image_data_lookup(
    archive: &DocxArchive,
) -> Result<HashMap<String, String>, RuntimeError> {
    use base64::Engine;

    let mut lookup = HashMap::new();

    // Collect all relationship file paths in word/_rels/
    let rels_files = collect_word_rels_files(archive);

    for rels_path in rels_files {
        // collect_word_rels_files enumerates paths from the archive's own
        // file listing, so `get` should always succeed here. If it doesn't,
        // something is seriously wrong with the archive internals — but this
        // is not a parse/validation failure, so we skip with a trace log.
        let Some(rels_xml) = archive.get(&rels_path) else {
            tracing::trace!(
                rels_path = %rels_path,
                "rels file listed in archive but not retrievable (skipping)"
            );
            continue;
        };
        let root = word_xml::parse_document_xml(rels_xml).map_err(|err| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!("failed to parse relationship file {rels_path}"),
            details: ErrorDetails {
                context: Some(format!("{err:?}")),
                ..ErrorDetails::default()
            },
        })?;

        // Determine the base path for resolving relative targets
        // e.g., "word/_rels/header1.xml.rels" → "word/"
        let base_path = rels_path
            .strip_prefix("word/_rels/")
            .and_then(|s| s.strip_suffix(".rels"))
            .map(|_| "word/")
            .unwrap_or("word/");

        for child in &root.children {
            let el = match child {
                XMLNode::Element(el) => el,
                _ => continue,
            };
            if local_element_name(el) != "Relationship" {
                continue;
            }
            let id = attr_get(el, "Id").ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!("{rels_path}: Relationship element missing required Id attribute"),
                details: ErrorDetails::default(),
            })?;
            let rel_type = attr_get(el, "Type").ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!("{rels_path}: Relationship {id} missing required Type attribute"),
                details: ErrorDetails::default(),
            })?;
            let target = attr_get(el, "Target").ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!(
                    "{rels_path}: Relationship {id} missing required Target attribute"
                ),
                details: ErrorDetails::default(),
            })?;

            // Only process image relationship types
            if rel_type
                != "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
            {
                continue;
            }

            // Resolve the target path relative to the base directory, applying
            // OPC pack-URI resolution (leading '/', '.'/'..' collapse). A wild
            // shape stores media at the PACKAGE ROOT and references it from
            // word/_rels/*.rels as "../media/image1.png" — without collapsing
            // ".." the lookup would miss the part and drop the image.
            let archive_path = resolve_relationship_target(target, base_path);

            // Missing image blob is a data-quality issue (the rels parsed
            // correctly, but the referenced file is absent). Warn and skip —
            // the relationship metadata itself is valid.
            let Some(image_bytes) = archive.get(&archive_path) else {
                tracing::warn!(
                    relationship_id = %id,
                    rels_file = %rels_path,
                    target = %target,
                    archive_path = %archive_path,
                    "image relationship target missing from DOCX archive; image cannot be rendered"
                );
                continue;
            };

            let mime = mime_from_extension(&archive_path);
            let b64 = base64::engine::general_purpose::STANDARD.encode(image_bytes);
            let data_uri = format!("data:{mime};base64,{b64}");

            lookup.insert(id.clone(), data_uri);
        }
    }

    tracing::debug!(
        image_count = lookup.len(),
        "built image data URI lookup from DOCX relationship files"
    );

    Ok(lookup)
}

/// Collect all relationship file paths in the word/_rels/ directory.
/// Returns paths like ["word/_rels/document.xml.rels", "word/_rels/header1.xml.rels", ...].
fn collect_word_rels_files(archive: &DocxArchive) -> Vec<String> {
    let files: Vec<String> = archive
        .list()
        .filter(|path| path.starts_with("word/_rels/") && path.ends_with(".xml.rels"))
        .map(|s| s.to_string())
        .collect();

    tracing::trace!(
        rels_file_count = files.len(),
        rels_files = ?files,
        "collected word relationship files"
    );

    files
}

/// Determine MIME type from file extension.
fn mime_from_extension(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("tiff" | "tif") => "image/tiff",
        Some("svg") => "image/svg+xml",
        Some("emf") => "image/x-emf",
        Some("wmf") => "image/x-wmf",
        _ => "application/octet-stream",
    }
}
/// Parse header/footer references from section properties in document.xml.
pub(crate) fn parse_header_footer_refs(
    root: &Element,
) -> Result<(Vec<HeaderFooterRef>, Vec<HeaderFooterRef>), RuntimeError> {
    let mut header_refs = Vec::new();
    let mut footer_refs = Vec::new();

    // Find sectPr elements (in body or directly in document)
    collect_sect_pr_refs(root, &mut header_refs, &mut footer_refs)?;

    header_refs.sort_by(|a, b| a.rel_id.cmp(&b.rel_id));
    footer_refs.sort_by(|a, b| a.rel_id.cmp(&b.rel_id));

    // Deduplicate by rel_id (multiple sections can reference same header/footer)
    header_refs.dedup_by(|a, b| a.rel_id == b.rel_id);
    footer_refs.dedup_by(|a, b| a.rel_id == b.rel_id);

    Ok((header_refs, footer_refs))
}

/// Recursively collect header/footer references from sectPr elements.
fn collect_sect_pr_refs(
    element: &Element,
    header_refs: &mut Vec<HeaderFooterRef>,
    footer_refs: &mut Vec<HeaderFooterRef>,
) -> Result<(), RuntimeError> {
    let local_name = local_element_name(element);

    if local_name == "sectPr" {
        // Parse header/footer references within this sectPr
        for child in &element.children {
            let el = match child {
                XMLNode::Element(el) => el,
                _ => continue,
            };
            let child_local = local_element_name(el);

            if child_local == "headerReference" {
                if let Some(hf_ref) = parse_header_footer_ref(el)? {
                    header_refs.push(hf_ref);
                }
            } else if child_local == "footerReference"
                && let Some(hf_ref) = parse_header_footer_ref(el)?
            {
                footer_refs.push(hf_ref);
            }
        }
    }

    // Recurse into children
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            collect_sect_pr_refs(el, header_refs, footer_refs)?;
        }
    }

    Ok(())
}
/// Extract w:sectPrChange from a body-level w:sectPr element.
///
/// Looks for a `sectPrChange` child, extracts revision metadata (id, author, date)
/// and the previous section properties as raw XML bytes.
///
/// Returns `Ok(None)` when there is no `sectPrChange` child (normal case).
/// Returns `Err` if the child exists but cannot be serialized.
pub(crate) fn extract_body_section_property_change(
    sect_pr: &Element,
) -> Result<Option<SectionPropertyChange>, RuntimeError> {
    let change_el = match sect_pr.children.iter().find_map(|child| {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => return None,
        };
        if is_w_tag(el, "sectPrChange") {
            Some(el)
        } else {
            None
        }
    }) {
        Some(el) => el,
        None => return Ok(None),
    };
    let revision_id = match attr_get(change_el, "id").and_then(|v| v.parse().ok()) {
        Some(id) => id,
        None => return Ok(None),
    };
    let author = attr_get(change_el, "author").cloned();
    let date = attr_get(change_el, "date").cloned();
    let prev_sect_pr = match change_el.children.iter().find_map(|child| {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => return None,
        };
        if is_w_tag(el, "sectPr") {
            Some(el)
        } else {
            None
        }
    }) {
        Some(el) => el,
        None => return Ok(None),
    };
    let mut buf = Vec::new();
    prev_sect_pr.write(&mut buf).map_err(|err| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: "failed to serialize previous section properties in sectPrChange".to_string(),
        details: ErrorDetails {
            context: Some(format!("{err}")),
            ..ErrorDetails::default()
        },
    })?;
    Ok(Some(SectionPropertyChange {
        revision: RevisionInfo {
            revision_id,
            author,
            date,
            apply_op_id: None,
        },
        previous_properties_raw: buf,
    }))
}
/// Parse a single headerReference or footerReference element.
fn parse_header_footer_ref(element: &Element) -> Result<Option<HeaderFooterRef>, RuntimeError> {
    // Get r:id attribute (relationship ID)
    let rel_id = match attr_get(element, "r:id")
        .or_else(|| attr_get(element, "id"))
        .cloned()
    {
        Some(id) => id,
        None => return Ok(None),
    };

    // Get w:type attribute for kind. §17.10.4: shared with word_ir.rs's
    // sectPr headerReference/footerReference parsing via
    // parse_header_footer_kind (single source of truth for the mapping,
    // including the unrecognized-w:type degradation — see its doc comment).
    let kind = crate::word_ir::parse_header_footer_kind(
        attr_get(element, "w:type")
            .or_else(|| attr_get(element, "type"))
            .map(|s| s.as_str()),
    );

    Ok(Some(HeaderFooterRef { rel_id, kind }))
}
/// The diagnostic recorded when a referenced running-head part is empty (0-byte
/// or whitespace-only) and imported as an empty running head. Word emits and
/// opens such parts without repair; we surface the tolerance rather than absorb
/// it silently.
fn empty_running_head_diagnostic(part_path: &str) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Info,
        message: format!(
            "tolerated empty running-head part {part_path}: a 0-byte / \
             whitespace-only header/footer part carries no w:hdr/w:ftr root, but \
             Word opens such documents without repair and renders no running head \
             — imported as an empty running head (no blocks)"
        ),
        context: Some(part_path.to_string()),
    }
}

/// Parse header stories from archive using relationships and refs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn parse_headers(
    archive: &DocxArchive,
    rels: &DocumentRelationships,
    header_refs: &[HeaderFooterRef],
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
    main_dir: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<HeaderStory>, RuntimeError> {
    let mut headers = Vec::new();

    for rel in &rels.headers {
        // Find the kind for this relationship.
        // Relationships without a matching headerReference in any sectPr are
        // orphaned (e.g., from a deleted section) — skip them.
        let Some(kind) = header_refs
            .iter()
            .find(|r| r.rel_id == rel.id)
            .map(|r| r.kind.clone())
        else {
            continue;
        };

        let part_name = rel.target.clone();
        let part_path = resolve_relationship_target(&part_name, main_dir);

        let Some(xml_bytes) = archive.get(&part_path) else {
            continue;
        };

        // A referenced but empty (0-byte / whitespace-only) part is a
        // Word-tolerated empty running head — the reference resolves to an empty
        // header. Import it as an empty story, not a parse failure. A part with
        // content but no root is malformed and still fails loud below.
        if word_xml::is_empty_or_whitespace_xml(xml_bytes) {
            diagnostics.push(empty_running_head_diagnostic(&part_path));
            headers.push(HeaderStory {
                part_name,
                kind,
                blocks: Vec::new(),
                content_hash: compute_story_content_hash(&[] as &[BlockNode]),
                synthesized: false,
            });
            continue;
        }

        // A part that is present but unparseable is a corrupt document, not an
        // absent header — fail loud (missing != malformed). See P0 #11.
        let root = word_xml::parse_document_xml(xml_bytes).map_err(|err| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!("failed to parse header part {part_path}"),
            details: ErrorDetails {
                context: Some(format!("{err:?}")),
                ..ErrorDetails::default()
            },
        })?;

        // Parse blocks from the header (hdr element is similar to body)
        let (parsed_blocks, sdt_wraps) =
            parse_story_blocks(&root, numbering_defs, style_defs, default_tab_stop)?;
        let content_hash = compute_story_content_hash(&parsed_blocks);
        let mut blocks: Vec<_> = parsed_blocks
            .into_iter()
            .map(normal_tracked_block)
            .collect();
        for (idx, wrap) in sdt_wraps {
            blocks[idx].block_sdt_wrap = Some(wrap);
        }

        headers.push(HeaderStory {
            part_name,
            kind,
            blocks,
            content_hash,
            synthesized: false,
        });
    }

    Ok(headers)
}

/// Parse footer stories from archive using relationships and refs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn parse_footers(
    archive: &DocxArchive,
    rels: &DocumentRelationships,
    footer_refs: &[HeaderFooterRef],
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
    main_dir: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<FooterStory>, RuntimeError> {
    let mut footers = Vec::new();

    for rel in &rels.footers {
        // Find the kind for this relationship.
        // Relationships without a matching footerReference in any sectPr are
        // orphaned (e.g., from a deleted section) — skip them.
        let Some(kind) = footer_refs
            .iter()
            .find(|r| r.rel_id == rel.id)
            .map(|r| r.kind.clone())
        else {
            continue;
        };

        let part_name = rel.target.clone();
        let part_path = resolve_relationship_target(&part_name, main_dir);

        let Some(xml_bytes) = archive.get(&part_path) else {
            continue;
        };

        // A referenced but empty (0-byte / whitespace-only) footer part is a
        // Word-tolerated empty running head (see `parse_headers`). Import it as
        // an empty story, not a parse failure.
        if word_xml::is_empty_or_whitespace_xml(xml_bytes) {
            diagnostics.push(empty_running_head_diagnostic(&part_path));
            footers.push(FooterStory {
                part_name,
                kind,
                blocks: Vec::new(),
                content_hash: compute_story_content_hash(&[] as &[BlockNode]),
                synthesized: false,
            });
            continue;
        }

        // Present-but-unparseable footer part is corrupt, not absent (P0 #11).
        let root = word_xml::parse_document_xml(xml_bytes).map_err(|err| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!("failed to parse footer part {part_path}"),
            details: ErrorDetails {
                context: Some(format!("{err:?}")),
                ..ErrorDetails::default()
            },
        })?;

        let (parsed_blocks, sdt_wraps) =
            parse_story_blocks(&root, numbering_defs, style_defs, default_tab_stop)?;
        let content_hash = compute_story_content_hash(&parsed_blocks);
        let mut blocks: Vec<_> = parsed_blocks
            .into_iter()
            .map(normal_tracked_block)
            .collect();
        for (idx, wrap) in sdt_wraps {
            blocks[idx].block_sdt_wrap = Some(wrap);
        }

        footers.push(FooterStory {
            part_name,
            kind,
            blocks,
            content_hash,
            synthesized: false,
        });
    }

    Ok(footers)
}
/// Parse footnotes from word/footnotes.xml.
pub(crate) fn parse_footnotes(
    archive: &DocxArchive,
    rels: &DocumentRelationships,
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
    main_dir: &str,
) -> Result<Vec<FootnoteStory>, RuntimeError> {
    let Some(rel) = &rels.footnotes else {
        return Ok(Vec::new());
    };

    let part_path = resolve_relationship_target(&rel.target, main_dir);

    let Some(xml_bytes) = archive.get(&part_path) else {
        return Ok(Vec::new());
    };

    // Present-but-unparseable footnotes part is corrupt, not absent (P0 #11).
    let root = word_xml::parse_document_xml(xml_bytes).map_err(|err| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("failed to parse footnotes part {part_path}"),
        details: ErrorDetails {
            context: Some(format!("{err:?}")),
            ..ErrorDetails::default()
        },
    })?;

    let mut footnotes = Vec::new();

    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        let local_name = local_element_name(el);
        if local_name != "footnote" {
            continue;
        }

        // Get note ID
        let id = attr_get(el, "w:id")
            .or_else(|| attr_get(el, "id"))
            .cloned()
            .unwrap_or_default();

        // Get note type - IMPORTANT: use type attribute, not ID
        let type_str = attr_get(el, "w:type")
            .or_else(|| attr_get(el, "type"))
            .map(|s| s.as_str())
            .unwrap_or("normal");

        let note_type = match type_str {
            "separator" => NoteType::Separator,
            "continuationSeparator" => NoteType::ContinuationSeparator,
            "continuationNotice" => NoteType::ContinuationNotice,
            _ => NoteType::Normal,
        };

        // Parse blocks from footnote
        let parsed_blocks = parse_note_blocks(el, numbering_defs, style_defs, default_tab_stop)?;
        let content_hash = compute_story_content_hash(&parsed_blocks);
        let blocks = parsed_blocks
            .into_iter()
            .map(normal_tracked_block)
            .collect();

        footnotes.push(FootnoteStory {
            id,
            note_type,
            blocks,
            content_hash,
        });
    }

    Ok(footnotes)
}

/// Parse endnotes from word/endnotes.xml.
pub(crate) fn parse_endnotes(
    archive: &DocxArchive,
    rels: &DocumentRelationships,
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
    main_dir: &str,
) -> Result<Vec<EndnoteStory>, RuntimeError> {
    let Some(rel) = &rels.endnotes else {
        return Ok(Vec::new());
    };

    let part_path = resolve_relationship_target(&rel.target, main_dir);

    let Some(xml_bytes) = archive.get(&part_path) else {
        return Ok(Vec::new());
    };

    // Present-but-unparseable endnotes part is corrupt, not absent (P0 #11).
    let root = word_xml::parse_document_xml(xml_bytes).map_err(|err| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("failed to parse endnotes part {part_path}"),
        details: ErrorDetails {
            context: Some(format!("{err:?}")),
            ..ErrorDetails::default()
        },
    })?;

    let mut endnotes = Vec::new();

    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        let local_name = local_element_name(el);
        if local_name != "endnote" {
            continue;
        }

        // Get note ID
        let id = attr_get(el, "w:id")
            .or_else(|| attr_get(el, "id"))
            .cloned()
            .unwrap_or_default();

        // Get note type - IMPORTANT: use type attribute, not ID
        let type_str = attr_get(el, "w:type")
            .or_else(|| attr_get(el, "type"))
            .map(|s| s.as_str())
            .unwrap_or("normal");

        let note_type = match type_str {
            "separator" => NoteType::Separator,
            "continuationSeparator" => NoteType::ContinuationSeparator,
            "continuationNotice" => NoteType::ContinuationNotice,
            _ => NoteType::Normal,
        };

        let parsed_blocks = parse_note_blocks(el, numbering_defs, style_defs, default_tab_stop)?;
        let content_hash = compute_story_content_hash(&parsed_blocks);
        let blocks = parsed_blocks
            .into_iter()
            .map(normal_tracked_block)
            .collect();

        endnotes.push(EndnoteStory {
            id,
            note_type,
            blocks,
            content_hash,
        });
    }

    Ok(endnotes)
}

/// Parse comments from word/comments.xml.
pub(crate) fn parse_comments(
    archive: &DocxArchive,
    rels: &DocumentRelationships,
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
    main_dir: &str,
) -> Result<Vec<CommentStory>, RuntimeError> {
    let Some(rel) = &rels.comments else {
        return Ok(Vec::new());
    };

    let part_path = resolve_relationship_target(&rel.target, main_dir);

    let Some(xml_bytes) = archive.get(&part_path) else {
        return Ok(Vec::new());
    };

    // Present-but-unparseable comments part is corrupt, not absent (P0 #11).
    let root = word_xml::parse_document_xml(xml_bytes).map_err(|err| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("failed to parse comments part {part_path}"),
        details: ErrorDetails {
            context: Some(format!("{err:?}")),
            ..ErrorDetails::default()
        },
    })?;

    let mut comments = Vec::new();

    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        let local_name = local_element_name(el);
        if local_name != "comment" {
            continue;
        }

        // Get comment ID
        let id = attr_get(el, "w:id")
            .or_else(|| attr_get(el, "id"))
            .cloned()
            .unwrap_or_default();

        // Get author
        let author = attr_get(el, "w:author")
            .or_else(|| attr_get(el, "author"))
            .cloned();

        // Get date
        let date = attr_get(el, "w:date")
            .or_else(|| attr_get(el, "date"))
            .cloned();

        let parsed_blocks = parse_note_blocks(el, numbering_defs, style_defs, default_tab_stop)?;
        let content_hash = compute_story_content_hash(&parsed_blocks);
        let blocks = parsed_blocks
            .into_iter()
            .map(normal_tracked_block)
            .collect();

        comments.push(CommentStory {
            id,
            author,
            date,
            blocks,
            content_hash,
            tracking_status: None,
        });
    }

    Ok(comments)
}

/// Parse `word/commentsExtended.xml` (MS-DOCX §2.5.1) into typed
/// [`CommentExtended`] records. Each `w15:commentEx` carries `w15:paraId`
/// (the comment's first-body-paragraph id), an optional `w15:paraIdParent`
/// (reply threading), and `w15:done` (resolved flag).
///
/// Replaces the previous opaque byte-passthrough: the part is now modeled so
/// the reply / resolve verbs can author and mutate it. A missing relationship
/// or missing part yields an empty vec (a document simply has no extended
/// comment metadata) — that is the contract, not a silent fallback for a
/// malformed part. A `w15:commentEx` with no `w15:paraId` is malformed
/// (the paraId is the record's identity) and is refused.
pub(crate) fn parse_comments_extended(
    archive: &DocxArchive,
    rels: &DocumentRelationships,
    main_dir: &str,
) -> Result<Vec<CommentExtended>, RuntimeError> {
    // The part may be referenced by relationship or live at the conventional
    // path. Prefer the relationship target; fall back to the well-known path
    // (relative to the main part's directory).
    let part_path = match &rels.comments_extended {
        Some(rel) => resolve_relationship_target(&rel.target, main_dir),
        None => format!("{main_dir}commentsExtended.xml"),
    };

    let Some(xml_bytes) = archive.get(&part_path) else {
        return Ok(Vec::new());
    };

    let root = word_xml::parse_document_xml(xml_bytes).map_err(|err| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("failed to parse {part_path}"),
        details: ErrorDetails {
            context: Some(format!("{err:?}")),
            ..ErrorDetails::default()
        },
    })?;

    let mut records = Vec::new();
    for child in &root.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        if local_element_name(el) != "commentEx" {
            continue;
        }
        // paraId is the record's identity (it keys back to a comment's first
        // body paragraph). A commentEx without it is malformed — fail loud.
        let para_id = attr_get(el, "w15:paraId")
            .or_else(|| attr_get(el, "paraId"))
            .cloned()
            .ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!("{part_path}: w15:commentEx missing required w15:paraId"),
                details: ErrorDetails::default(),
            })?;
        let para_id_parent = attr_get(el, "w15:paraIdParent")
            .or_else(|| attr_get(el, "paraIdParent"))
            .cloned();
        let done = attr_get(el, "w15:done")
            .or_else(|| attr_get(el, "done"))
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        records.push(CommentExtended {
            para_id,
            para_id_parent,
            done,
        });
    }

    Ok(records)
}
/// Blocks parsed from a story root plus the recovered story-level SDT
/// envelopes as (block index, wrapper) pairs — applied to the TrackedBlocks by
/// the header/footer assemblers.
type ParsedStoryBlocks = (Vec<BlockNode>, Vec<(usize, BlockSdtWrap)>);

/// Parse blocks from a story root element (hdr, ftr).
/// Uses synthetic IDs since story paragraphs don't have anchors.
fn parse_story_blocks(
    root: &Element,
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
) -> Result<ParsedStoryBlocks, RuntimeError> {
    let mut blocks = Vec::new();
    // (block index, envelope) for story-level `w:sdt` wrappers recovered during
    // parsing; applied to the TrackedBlocks by the header/footer assemblers.
    let mut sdt_wraps: Vec<(usize, BlockSdtWrap)> = Vec::new();
    let mut block_counter = 1u32;
    let mut inline_counter = 1u32;
    let mut table_counter = 1u32;
    let mut opaque_counter = 1u32;
    let mut block_id_counter = 1u32;
    let mut diagnostics = Vec::new();
    let mut numbering_state = crate::numbering::NumberingState::new();
    let default_compat = CompatSettings::default();
    let empty_rel_lookup = HashMap::new();
    let mut ctx = ParseContext {
        diagnostics: &mut diagnostics,
        opaque_counter: &mut opaque_counter,
        inline_counter: &mut inline_counter,
        block_id_counter: &mut block_id_counter,
        numbering_defs,
        numbering_state: &mut numbering_state,
        style_defs,
        default_tab_stop,
        compat_settings: &default_compat,
        rel_lookup: &empty_rel_lookup,
        active_move_name: None,
        active_move_status: None,
    };

    // Story-root-level bookmark markers (between paragraphs in w:hdr/w:ftr;
    // §17.13.2 cross-structure annotations), re-anchored at the nearest
    // paragraph boundary like the table-structure markers.
    let mut pending_markers: Vec<(usize, InlineNode)> = Vec::new();

    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        if is_w_tag(el, "bookmarkStart") || is_w_tag(el, "bookmarkEnd") {
            pending_markers.push((
                blocks.len(),
                structural_bookmark_decoration(el, "story", &mut ctx),
            ));
            continue;
        }

        // Use story-specific block parsing with synthetic IDs. Wrappers
        // (sdt/customXml/ins/del/AlternateContent) are descended into rather than
        // dropped (P0 #10).
        append_story_blocks_from_element(
            el,
            &mut block_counter,
            &mut table_counter,
            &mut ctx,
            &mut blocks,
            &mut sdt_wraps,
            false,
        )?;
    }

    attach_structural_markers_to_blocks(&mut blocks, pending_markers, ctx.diagnostics, "story");

    Ok((blocks, sdt_wraps))
}

/// Parse a story child element, appending the resulting block(s) to `out`.
///
/// `parse_story_block` only recognizes `w:p` and `w:tbl`. Every other element
/// used to fall through to its `Ok(None)` tail and be silently dropped — an
/// SDT-wrapped header paragraph (Word's page-number gallery), a body-level
/// `w:ins`/`w:del` block in a footnote, an `mc:AlternateContent`, etc.
/// disappeared with no diagnostic (P0 #10). This wrapper recovers the content of
/// transparent / block-level wrappers and surfaces a diagnostic for anything it
/// still cannot represent, so nothing is dropped silently.
fn append_story_blocks_from_element(
    element: &Element,
    block_counter: &mut u32,
    table_counter: &mut u32,
    ctx: &mut ParseContext,
    out: &mut Vec<BlockNode>,
    sdt_wraps: &mut Vec<(usize, BlockSdtWrap)>,
    inside_sdt: bool,
) -> Result<(), RuntimeError> {
    // Paragraphs and tables: the existing parser handles these directly.
    if let Some(block) = parse_story_block(element, block_counter, table_counter, ctx)? {
        out.push(block);
        return Ok(());
    }

    // w:sdt — descend into w:sdtContent (repeating sections, galleries, etc.)
    // AND preserve the envelope: the recovered blocks are recorded as a
    // `BlockSdtWrap` range (the same model `WrapBlocksInContentControl`
    // authors), so the `w:sdtPr` (Word's page-number gallery `docPartObj`,
    // repeating-section bindings, …) survives the story-part rebuild instead of
    // being silently unwrapped. Only the OUTERMOST envelope is representable —
    // `block_sdt_wrap` ranges must not overlap — so a nested story SDT is still
    // flattened, with a diagnostic instead of silence.
    if is_w_tag(element, "sdt") {
        let start = out.len();
        for child in &element.children {
            if let XMLNode::Element(content) = child
                && is_w_tag(content, "sdtContent")
            {
                for inner in &content.children {
                    if let XMLNode::Element(ie) = inner {
                        append_story_blocks_from_element(
                            ie,
                            block_counter,
                            table_counter,
                            ctx,
                            out,
                            sdt_wraps,
                            true,
                        )?;
                    }
                }
            }
        }
        let span = out.len() - start;
        if inside_sdt {
            ctx.diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                message: "nested story-level <w:sdt> envelope flattened; content preserved \
                          (only the outermost story content-control wrapper is modeled)"
                    .to_string(),
                context: None,
            });
        } else if span >= 1 {
            sdt_wraps.push((
                start,
                BlockSdtWrap {
                    wrapper: extract_sdt_wrapper(element)?,
                    span,
                },
            ));
        } else {
            ctx.diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                message: "story-level <w:sdt> with no block content dropped".to_string(),
                context: None,
            });
        }
        return Ok(());
    }

    // mc:AlternateContent — select the branch we understand and recurse.
    if is_mc_alternate_content(element) {
        if let Some(branch) = select_mc_branch(element).map_err(map_word_ir_error)? {
            for inner in &branch.children {
                if let XMLNode::Element(ie) = inner {
                    append_story_blocks_from_element(
                        ie,
                        block_counter,
                        table_counter,
                        ctx,
                        out,
                        sdt_wraps,
                        inside_sdt,
                    )?;
                }
            }
        }
        return Ok(());
    }

    // Transparent / block-level tracked wrappers: customXml/smartTag are
    // transparent; w:ins/w:del/w:moveFrom/w:moveTo wrap whole blocks. Recover
    // the content so it is not lost. NOTE: story parts do not yet model
    // block-level tracking status (every story block is materialized Normal), so
    // the wrapper's insert/delete attribution is flattened here — recorded as a
    // diagnostic rather than dropped. Inline tracking within paragraphs is
    // preserved (see segments_from_tracked_atoms in parse_story_block).
    if is_w_tag(element, "customXml")
        || is_w_tag(element, "smartTag")
        || is_w_tag(element, "ins")
        || is_w_tag(element, "del")
        || is_w_tag(element, "moveFrom")
        || is_w_tag(element, "moveTo")
    {
        if is_w_tag(element, "ins")
            || is_w_tag(element, "del")
            || is_w_tag(element, "moveFrom")
            || is_w_tag(element, "moveTo")
        {
            ctx.diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                message: format!(
                    "story-level block-tracked <{}> wrapper flattened to Normal; content \
                     preserved (block-level story tracking is not yet modeled)",
                    element.name
                ),
                context: None,
            });
        }
        for inner in &element.children {
            if let XMLNode::Element(ie) = inner {
                append_story_blocks_from_element(
                    ie,
                    block_counter,
                    table_counter,
                    ctx,
                    out,
                    sdt_wraps,
                    inside_sdt,
                )?;
            }
        }
        return Ok(());
    }

    // w:sectPr is section properties, legitimately not content — skip quietly.
    // Anything else is genuinely unsupported here: surface it instead of
    // dropping it silently.
    if !is_w_tag(element, "sectPr") {
        ctx.diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Warning,
            message: format!("dropped unsupported story block element: {}", element.name),
            context: None,
        });
    }
    Ok(())
}

/// Parse a single block from a story element (hdr/ftr/footnote/etc).
/// Uses synthetic IDs since story paragraphs don't have internal anchors.
fn parse_story_block(
    element: &Element,
    block_counter: &mut u32,
    table_counter: &mut u32,
    ctx: &mut ParseContext,
) -> Result<Option<BlockNode>, RuntimeError> {
    // Compat-tolerance edge (see `crate::compat`): rewrite schema-invalid but
    // Word-accepted within-subtree shapes (w:shd w:val="none"; nested w:r) in
    // header/footer/footnote/comment stories too, so a story part is not
    // refused where the body would be tolerated. Gated on a read-only pre-check.
    let compat_owned;
    let element = if crate::compat::subtree_has_tolerated_shape(element) {
        compat_owned = crate::compat::normalize_tolerated_shapes(element, ctx.diagnostics);
        &compat_owned
    } else {
        element
    };

    // Handle w:p (paragraph)
    if is_w_tag(element, "p") {
        let para_id = attr_get(element, "w14:paraId").cloned();
        let text_id = attr_get(element, "w14:textId").cloned();
        let block_id = NodeId::from(format!("story_p{}", *block_counter));
        *block_counter += 1;

        let view = ParagraphView::from_paragraph(element, ctx.rel_lookup)
            .map_err(|e| invalid_docx(&format!("story paragraph {}: {}", block_id.0, e)))?;

        let block_text = view.block_text();
        let mut inlines = inline_nodes_from_atoms(
            &block_id,
            &view.atoms,
            ctx.inline_counter,
            ctx.style_defs,
            view.style_id.as_deref(),
        )
        .map_err(|e| invalid_docx(&format!("story paragraph {}: {}", block_id.0, e.message)))?;

        // Heading level: derived the ONE shared way (see derive_heading_level_number)
        // so the story path can never re-drift from the body. Computed here before
        // any field of `view` is moved out below. Unstyled paragraphs implicitly
        // reference the default paragraph style for outline resolution (§17.7.4.17).
        let effective_heading_style_id: Option<&str> = view
            .style_id
            .as_deref()
            .or_else(|| ctx.style_defs.and_then(|sd| sd.default_para_style_id()));
        let heading_level =
            derive_heading_level_number(&view, effective_heading_style_id, ctx.style_defs);

        // Capture the inline count before prefix stripping so we can offset into
        // `view.atoms` when building tracked segments below (atoms and inlines
        // are 1:1 before stripping).
        let pre_strip_count = inlines.len();

        // Strip typed numbering prefix from inlines — unless the label text
        // is tracked (then it must stay in the body as a tracked segment so
        // accept/reject can resolve it).
        let tracked_flags: Vec<bool> = view.atoms.iter().map(|a| a.tracking.is_some()).collect();
        let (
            literal_prefix,
            literal_prefix_leading_tab_count,
            literal_prefix_has_leading_tab,
            literal_prefix_has_trailing_tab,
            literal_prefix_leading_ws,
            literal_prefix_trailing_ws,
            literal_prefix_marks,
            literal_prefix_style_props,
            literal_prefix_rpr_authored,
            literal_prefix_leading_rpr,
            literal_prefix_trailing_rpr,
        ) = match strip_literal_prefix_with_tracked_flags(&mut inlines, &tracked_flags) {
            Some(prefix) => (
                Some(prefix.label),
                prefix.leading_tab_count,
                prefix.has_leading_tab,
                prefix.has_trailing_tab,
                prefix.leading_ws,
                prefix.trailing_ws,
                prefix.marks,
                prefix.style_props,
                prefix.rpr_authored,
                prefix.leading_rpr.map(Box::new),
                prefix.trailing_rpr.map(Box::new),
            ),
            None => (
                None,
                0,
                false,
                false,
                String::new(),
                String::new(),
                Vec::new(),
                StyleProps::default(),
                RunRprAuthored::default(),
                None,
                None,
            ),
        };
        // Number of inlines removed by prefix stripping — skip those atoms when
        // pairing the remaining atoms with inlines for segment building.
        let prefix_len = pre_strip_count - inlines.len();
        let body_text = extract_inline_text_simple(&inlines);

        // Resolve effective numPr: direct wins, else style-resolved (§17.7.4.14).
        // DirectNumPr::Suppressed (numId=0, §17.9.18) blocks style and pStyle binding.
        let mut effective_num_props = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_num_props(view.style_id.as_deref(), &view.num_props)
        } else {
            match &view.num_props {
                crate::word_ir::DirectNumPr::Active(np) => Some(np.clone()),
                _ => None,
            }
        };

        // §17.9.23: pStyle reverse binding — if no numPr from direct or style,
        // check if any numbering level claims this paragraph's style via <w:pStyle>.
        // Only applies when numPr is truly absent — Suppressed (numId=0) blocks this.
        if effective_num_props.is_none()
            && view.num_props == crate::word_ir::DirectNumPr::Absent
            && let (Some(style_id), Some(defs)) = (view.style_id.as_deref(), ctx.numbering_defs)
        {
            let pstyle_map = defs.build_pstyle_reverse_map();
            if let Some(&(num_id, ilvl)) = pstyle_map.get(style_id) {
                effective_num_props = Some(crate::word_ir::NumProps { num_id, ilvl });
            }
        }

        // Synthesize numbering if available
        let (numbering, rendered_text) = match (&effective_num_props, ctx.numbering_defs) {
            (Some(num_props), Some(defs)) => {
                match ctx
                    .numbering_state
                    .synthesize(defs, num_props.num_id, num_props.ilvl)
                {
                    Ok(synthesized) => {
                        let is_bullet = defs
                            .get_level(num_props.num_id, num_props.ilvl)
                            .is_some_and(|l| l.num_fmt == crate::numbering::NumFormat::Bullet);
                        let numbering_info = crate::domain::NumberingInfo {
                            num_id: num_props.num_id,
                            ilvl: num_props.ilvl,
                            synthesized_text: synthesized.clone(),
                            is_bullet,
                            restart_numbering: false,
                        };
                        // Use the level's suffix (§17.9.28) instead of hardcoding tab.
                        let rendered = if synthesized.is_empty() {
                            None
                        } else {
                            let sep = defs
                                .get_level(num_props.num_id, num_props.ilvl)
                                .map(|l| l.suffix.separator())
                                .unwrap_or("\t");
                            Some(format!("{synthesized}{sep}{body_text}"))
                        };
                        (Some(numbering_info), rendered)
                    }
                    Err(err) => {
                        // OBSERVABLE DEGRADATION BOUNDARY — same as the body
                        // paragraph path (paragraph_from_element): a dangling
                        // numId/ilvl demotes this story paragraph from list
                        // item to plain paragraph rather than refusing the
                        // whole import (invariant #1, parse totality). Kept
                        // observable rather than silent.
                        tracing::warn!(
                            block_id = %block_id.0,
                            num_id = num_props.num_id,
                            ilvl = num_props.ilvl,
                            error = %err,
                            "numbering synthesis failed; demoting story paragraph to plain text (literal prefix fallback)"
                        );
                        if let Some(ref lp) = literal_prefix {
                            (None, Some(format!("{lp}\t{body_text}")))
                        } else {
                            (None, None)
                        }
                    }
                }
            }
            _ => {
                if let Some(ref lp) = literal_prefix {
                    (None, Some(format!("{lp}\t{body_text}")))
                } else {
                    (None, None)
                }
            }
        };

        // Resolve alignment and indentation through style chain for story paragraphs
        let resolved_alignment = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_alignment(view.style_id.as_deref(), view.alignment.as_deref())
        } else {
            view.alignment.clone()
        };
        let numbering_level_indent = effective_num_props
            .as_ref()
            .and_then(|np| ctx.numbering_defs?.get_level(np.num_id, np.ilvl))
            .and_then(|level| level.indent.as_ref());
        let resolved_indent = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_indent(
                view.style_id.as_deref(),
                view.indentation.as_ref(),
                numbering_level_indent,
            )
        } else {
            let left = view
                .indentation
                .as_ref()
                .and_then(|d| d.left)
                .or_else(|| numbering_level_indent.and_then(|n| n.left));
            let right = view
                .indentation
                .as_ref()
                .and_then(|d| d.right)
                .or_else(|| numbering_level_indent.and_then(|n| n.right));
            let first_line = view
                .indentation
                .as_ref()
                .and_then(|d| d.effective_first_line_twips)
                .or_else(|| numbering_level_indent.and_then(|n| n.effective_first_line_twips));
            // Character-unit indents come from the direct w:ind (numbering's
            // LevelIndent is twips-only). Preserve them — including an explicit
            // 0, which is a real override — instead of dropping to None.
            let start_chars = view.indentation.as_ref().and_then(|d| d.start_chars);
            let end_chars = view.indentation.as_ref().and_then(|d| d.end_chars);
            let first_line_chars = view.indentation.as_ref().and_then(|d| d.first_line_chars);
            let hanging_chars = view.indentation.as_ref().and_then(|d| d.hanging_chars);
            if left.is_some()
                || right.is_some()
                || first_line.is_some()
                || start_chars.is_some()
                || end_chars.is_some()
                || first_line_chars.is_some()
                || hanging_chars.is_some()
            {
                Some(crate::word_ir::IndentProps {
                    left,
                    right,
                    effective_first_line_twips: first_line,
                    start_chars,
                    end_chars,
                    first_line_chars,
                    hanging_chars,
                })
            } else {
                None
            }
        };
        let resolved_spacing = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_spacing(view.style_id.as_deref(), view.spacing.as_ref())
        } else {
            view.spacing.clone()
        };
        let resolved_borders = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_borders(view.style_id.as_deref(), view.borders.as_ref())
        } else {
            view.borders.clone()
        };
        let contextual_spacing = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_contextual_spacing(
                view.style_id.as_deref(),
                view.contextual_spacing,
            )
        } else {
            view.contextual_spacing
        };
        let widow_control = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_widow_control(view.style_id.as_deref(), view.widow_control)
        } else {
            view.widow_control
        };
        let keep_next = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_keep_next(view.style_id.as_deref(), view.keep_next)
        } else {
            view.keep_next
        };
        let keep_lines = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_keep_lines(view.style_id.as_deref(), view.keep_lines)
        } else {
            view.keep_lines
        };
        let page_break_before = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_page_break_before(view.style_id.as_deref(), view.page_break_before)
        } else {
            view.page_break_before
        }
        .unwrap_or(false);

        // MS-OI29500 2.1.45 §17.3.1.13: Word defaults to Left alignment.
        let align = Some(
            resolved_alignment
                .as_ref()
                .map(|a| match a.as_str() {
                    "left" | "start" => Ok(Alignment::Left),
                    "center" => Ok(Alignment::Center),
                    "right" | "end" => Ok(Alignment::Right),
                    "both" | "justify" => Ok(Alignment::Justify),
                    "distribute" => Ok(Alignment::Distribute),
                    "highKashida" => Ok(Alignment::HighKashida),
                    "lowKashida" => Ok(Alignment::LowKashida),
                    "mediumKashida" => Ok(Alignment::MediumKashida),
                    "numTab" => Ok(Alignment::NumTab),
                    "thaiDistribute" => Ok(Alignment::ThaiDistribute),
                    other => Err(invalid_docx(&format!(
                        "story paragraph {}: jc: unrecognized alignment value {:?}",
                        block_id.0, other
                    ))),
                })
                .transpose()?
                .unwrap_or(Alignment::Left),
        ); // absent → spec default

        // Resolve and synthesize tab stops for story paragraphs
        let resolved_stops = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_tabs(view.style_id.as_deref(), view.tab_stops.as_deref())
        } else {
            view.tab_stops
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter(|ts| ts.alignment != crate::domain::TabAlignment::Clear)
                .cloned()
                .collect()
        };
        let left_indent_twips = resolved_indent.as_ref().and_then(|i| i.left).unwrap_or(0);
        let first_line_twips = resolved_indent
            .as_ref()
            .and_then(|i| i.effective_first_line_twips)
            .unwrap_or(0);
        let effective_edge = left_indent_twips + first_line_twips;
        let body_tab_count = body_text.matches('\t').count();
        let prefix_had_tab = literal_prefix_has_leading_tab || literal_prefix_has_trailing_tab;
        let tab_stops_abs = crate::word_ir::synthesize_default_tab_stops(
            &resolved_stops,
            body_tab_count + usize::from(prefix_had_tab),
            ctx.default_tab_stop,
            effective_edge,
        );
        let body_left = if prefix_had_tab {
            tab_stops_abs
                .iter()
                .find(|s| s.position > effective_edge)
                .map(|s| s.position)
                .unwrap_or(effective_edge)
        } else {
            effective_edge
        };
        let leading_tab_gap_twips: Option<i32> = if literal_prefix_has_leading_tab {
            Some(body_left - left_indent_twips)
        } else {
            None
        };
        let consumed_prefix_tab_stop_twips: Option<i32> =
            if prefix_had_tab && resolved_stops.iter().any(|s| s.position == body_left) {
                Some(body_left - left_indent_twips)
            } else {
                None
            };

        // DERIVED view value (see the body-paragraph twin for the full note):
        // effective stops made body-left-relative for the frontend. The
        // serializer re-emits only the AUTHORED `view.tab_stops` verbatim.
        let effective_tab_stops_rel: Vec<_> = tab_stops_abs
            .into_iter()
            .map(|mut s| {
                s.position -= body_left;
                s
            })
            .filter(|s| s.position > 0)
            .collect();
        let tab_stops: Vec<_> = view.tab_stops.clone().unwrap_or_default();

        // Convert resolved indentation, absorbing first_line for tabbed paragraphs.
        let indent = if literal_prefix.is_some() {
            Some(Indentation {
                left: Some(left_indent_twips),
                right: resolved_indent.as_ref().and_then(|i| i.right),
                effective_first_line_twips: resolved_indent
                    .as_ref()
                    .and_then(|i| i.effective_first_line_twips)
                    .map(|_| first_line_twips),
                start_chars: None,
                end_chars: None,
                first_line_chars: None,
                hanging_chars: None,
            })
        } else if body_tab_count > 0 && first_line_twips != 0 {
            Some(Indentation {
                left: Some(effective_edge),
                right: resolved_indent.as_ref().and_then(|i| i.right),
                effective_first_line_twips: None,
                start_chars: None,
                end_chars: None,
                first_line_chars: None,
                hanging_chars: None,
            })
        } else {
            resolved_indent.as_ref().map(|i| Indentation {
                left: i.left,
                right: i.right,
                effective_first_line_twips: i.effective_first_line_twips,
                start_chars: i.start_chars,
                end_chars: i.end_chars,
                first_line_chars: i.first_line_chars,
                hanging_chars: i.hanging_chars,
            })
        };

        let spacing = resolved_spacing.map(|sp| ParagraphSpacing {
            before: sp.before,
            after: sp.after,
            before_lines: sp.before_lines,
            after_lines: sp.after_lines,
            before_autospacing: sp.before_autospacing,
            after_autospacing: sp.after_autospacing,
            line: sp.line,
            line_rule: sp.line_rule.as_deref().and_then(|r| match r {
                "auto" => Some(LineSpacingRule::Auto),
                "exact" => Some(LineSpacingRule::Exact),
                "atLeast" => Some(LineSpacingRule::AtLeast),
                _ => None,
            }),
        });
        let borders = convert_paragraph_borders_from_edges(resolved_borders)
            .map_err(|e| invalid_docx(&format!("story paragraph {}: {}", block_id.0, e.message)))?;

        // Convert direct paragraph shading
        let shading_authored = view.paragraph_shading.is_some();
        let direct_shading = match view.paragraph_shading {
            Some((fill, val, color)) => {
                let val = val
                    .as_deref()
                    .map(ShadingPattern::from_xml_str)
                    .transpose()
                    .map_err(|e| {
                        invalid_docx(&format!(
                            "story paragraph {}: paragraph shading: {e}",
                            block_id.0
                        ))
                    })?;
                Some(Shading {
                    fill,
                    val,
                    color,
                    extra_attrs: Vec::new(),
                })
            }
            None => None,
        };

        // Resolve shading through style chain (§17.3.1.31)
        let shading = if let Some(sd) = ctx.style_defs {
            sd.resolve_effective_para_shading(view.style_id.as_deref(), direct_shading.as_ref())
        } else {
            direct_shading
        };

        return Ok(Some(BlockNode::from(ParagraphNode {
            id: block_id,
            style_id: view.style_id.clone(),
            align,
            has_direct_align: view.alignment.is_some(),
            indent,
            has_direct_indent: view.indentation.is_some(),
            authored_indent: authored_indentation(view.indentation.as_ref()),
            spacing,
            has_direct_spacing: view.spacing.is_some(),
            authored_spacing: authored_paragraph_spacing(view.spacing.as_ref()),
            borders,
            keep_next,
            keep_lines,
            page_break_before,
            widow_control,
            contextual_spacing,
            shading,
            has_direct_keep_next: view.keep_next.is_some(),
            has_direct_keep_lines: view.keep_lines.is_some(),
            has_direct_page_break_before: view.page_break_before.is_some(),
            has_direct_widow_control: view.widow_control.is_some(),
            has_direct_contextual_spacing: view.contextual_spacing.is_some(),
            has_direct_shading: shading_authored,
            has_direct_borders: view.borders.is_some(),
            tab_stops,
            effective_tab_stops_rel,
            // Preserve tracked segments in story parts (headers/footers/footnotes/
            // endnotes/comments) exactly as the body path does. Previously this was
            // `normal_segment(inlines)`, which forced every story run to Normal — so
            // a footnote's `w:delText` was concatenated into visible text and its
            // attribution dropped (P0 #9: deleted text became live text). The body
            // builds segments the same way (see paragraph_from_element).
            segments: segments_from_tracked_atoms(&view.atoms[prefix_len..], inlines),
            block_text_hash: Some(sha256_hex(block_text.as_bytes())),
            numbering,
            // See paragraph_from_element: emit direct numPr only when authored.
            has_direct_numbering: matches!(view.num_props, crate::word_ir::DirectNumPr::Active(_)),
            numbering_suppressed: matches!(view.num_props, crate::word_ir::DirectNumPr::Suppressed),
            materialized_numbering: None,
            rendered_text,
            literal_prefix,
            literal_prefix_marks,
            literal_prefix_style_props,
            literal_prefix_rpr_authored,
            literal_prefix_leading_rpr,
            literal_prefix_trailing_rpr,
            literal_prefix_leading_tab_twips: leading_tab_gap_twips,
            literal_prefix_leading_tab_count,
            literal_prefix_leading_ws,
            literal_prefix_trailing_ws,
            literal_prefix_has_trailing_tab,
            literal_prefix_trailing_tab_stop_twips: consumed_prefix_tab_stop_twips,
            outline_lvl: view.outline_lvl,
            heading_level: heading_level.map(HeadingLevel::from_number),
            para_mark_status: view.para_mark_status,
            paragraph_mark_marks: convert_text_marks_to_marks(&view.paragraph_mark_rpr),
            paragraph_mark_style_props: convert_text_marks_to_style_props(
                &view.paragraph_mark_rpr,
            )?,
            paragraph_mark_rpr_off: convert_text_marks_to_para_mark_off(&view.paragraph_mark_rpr),
            para_split: false,
            section_property_change: view.section_property_change,
            formatting_change: view
                .ppr_change
                .as_ref()
                .map(convert_ppr_change)
                .transpose()?,
            section_properties: view.section_properties,
            mirror_indents: view.mirror_indents,
            auto_space_de: view.auto_space_de,
            auto_space_dn: view.auto_space_dn,
            bidi: view.bidi,
            text_alignment: view.text_alignment.clone(),
            suppress_auto_hyphens: view.suppress_auto_hyphens,
            snap_to_grid: view.snap_to_grid,
            overflow_punct: view.overflow_punct,
            adjust_right_ind: view.adjust_right_ind,
            word_wrap: view.word_wrap,
            frame_pr: view
                .frame_pr
                .as_ref()
                .map(|fp| crate::domain::FrameProperties {
                    width: fp.width,
                    height: fp.height,
                    h_rule: fp.h_rule.clone(),
                    h_space: fp.h_space,
                    v_space: fp.v_space,
                    wrap: fp.wrap.clone(),
                    v_anchor: fp.v_anchor.clone(),
                    h_anchor: fp.h_anchor.clone(),
                    x: fp.x,
                    x_align: fp.x_align.clone(),
                    y: fp.y,
                    y_align: fp.y_align.clone(),
                    extra_attrs: fp.extra_attrs.clone(),
                }),
            para_id,
            text_id,
            text_direction: view.text_direction.clone(),
            cnf_style: view.cnf_style.clone(),
            preserved_ppr: view.preserved.clone(),
        })));
    }

    // Handle w:tbl (table)
    if is_w_tag(element, "tbl") {
        let table = table_from_element(element, table_counter, ctx)?;
        return Ok(Some(BlockNode::from(table)));
    }

    // Skip other elements (sectPr, etc.)
    Ok(None)
}

/// Parse blocks from a footnote/endnote/comment element.
/// Uses synthetic IDs since notes don't have internal anchors.
fn parse_note_blocks(
    element: &Element,
    numbering_defs: Option<&crate::numbering::NumberingDefinitions>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
    default_tab_stop: i32,
) -> Result<Vec<BlockNode>, RuntimeError> {
    let mut blocks = Vec::new();
    let mut block_counter = 1u32;
    let mut inline_counter = 1u32;
    let mut table_counter = 1u32;
    let mut opaque_counter = 1u32;
    let mut block_id_counter = 1u32;
    let mut diagnostics = Vec::new();
    let mut numbering_state = crate::numbering::NumberingState::new();
    let default_compat2 = CompatSettings::default();
    let empty_rel_lookup = HashMap::new();
    let mut ctx = ParseContext {
        diagnostics: &mut diagnostics,
        opaque_counter: &mut opaque_counter,
        inline_counter: &mut inline_counter,
        block_id_counter: &mut block_id_counter,
        numbering_defs,
        numbering_state: &mut numbering_state,
        style_defs,
        default_tab_stop,
        compat_settings: &default_compat2,
        rel_lookup: &empty_rel_lookup,
        active_move_name: None,
        active_move_status: None,
    };

    // Note-root-level bookmark markers (between paragraphs in
    // w:footnote/w:endnote/w:comment), same re-anchoring as story roots.
    let mut pending_markers: Vec<(usize, InlineNode)> = Vec::new();
    let mut note_sdt_wraps: Vec<(usize, BlockSdtWrap)> = Vec::new();

    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        if is_w_tag(el, "bookmarkStart") || is_w_tag(el, "bookmarkEnd") {
            pending_markers.push((
                blocks.len(),
                structural_bookmark_decoration(el, "note", &mut ctx),
            ));
            continue;
        }

        // Use story-specific block parsing with synthetic IDs. Wrappers
        // (sdt/customXml/ins/del/AlternateContent) are descended into rather than
        // dropped (P0 #10).
        append_story_blocks_from_element(
            el,
            &mut block_counter,
            &mut table_counter,
            &mut ctx,
            &mut blocks,
            &mut note_sdt_wraps,
            false,
        )?;
    }

    // The note-part serializer (`sync_note_like_part`) does not emit
    // block-level SDT envelopes yet, so a note-level content-control wrapper
    // is flattened here — content preserved, envelope surfaced as a
    // diagnostic instead of silently dropped.
    if !note_sdt_wraps.is_empty() {
        ctx.diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Warning,
            message: format!(
                "note-level <w:sdt> envelope flattened; content preserved \
                 ({} wrapper(s); note parts do not model block content controls yet)",
                note_sdt_wraps.len()
            ),
            context: None,
        });
    }

    attach_structural_markers_to_blocks(&mut blocks, pending_markers, ctx.diagnostics, "note");

    Ok(blocks)
}
/// Compute a content hash for a story's blocks.
pub(crate) fn compute_story_content_hash(blocks: &[BlockNode]) -> String {
    let mut hasher = Sha256::new();
    for block in blocks {
        let text = extract_block_text(block);
        hasher.update(text.as_bytes());
        hasher.update(b"|");
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Extract the visible text of a block.
///
/// Used internally for block hashing and exposed publicly so read-view
/// consumers (e.g. the MCP server) can render a block's text without
/// reimplementing inline traversal. Pure function over already-validated IR.
pub fn extract_block_text(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(p) => {
            let mut out = String::new();
            for inline in p.all_inlines() {
                match inline {
                    InlineNode::Text(t) => out.push_str(&t.text),
                    InlineNode::HardBreak(_) => out.push('\n'),
                    InlineNode::OpaqueInline(_) => out.push('\u{FFFC}'),
                    InlineNode::Decoration(_) => {}
                    InlineNode::CommentRangeStart { .. }
                    | InlineNode::CommentRangeEnd { .. }
                    | InlineNode::CommentReference { .. } => {}
                }
            }
            out
        }
        BlockNode::Table(t) => {
            let mut out = String::new();
            for row in &t.rows {
                for cell in &row.cells {
                    for block in &cell.blocks {
                        out.push_str(&extract_block_text(block));
                        out.push(' ');
                    }
                }
            }
            out
        }
        BlockNode::OpaqueBlock(_) => String::new(),
    }
}
/// Build story payloads (footnotes, endnotes, comments) from both canonical docs.
/// Target takes precedence; base-only stories are included when not in target.
pub(crate) fn build_story_payloads(
    base: &CanonDoc,
    target: &CanonDoc,
    blocks: Vec<FullDocBlock>,
) -> FullDocViewResult {
    let target_fn_ids: HashSet<&str> = target
        .footnotes
        .iter()
        .filter(|n| n.note_type == NoteType::Normal)
        .map(|n| n.id.as_str())
        .collect();
    let mut footnotes: Vec<StoryPayload> = target
        .footnotes
        .iter()
        .filter(|n| n.note_type == NoteType::Normal)
        .map(|n| StoryPayload {
            id: n.id.clone(),
            segments: story_blocks_to_segments(&n.blocks),
        })
        .collect();
    footnotes.extend(
        base.footnotes
            .iter()
            .filter(|n| n.note_type == NoteType::Normal && !target_fn_ids.contains(n.id.as_str()))
            .map(|n| StoryPayload {
                id: n.id.clone(),
                segments: story_blocks_to_segments(&n.blocks),
            }),
    );

    let target_en_ids: HashSet<&str> = target
        .endnotes
        .iter()
        .filter(|n| n.note_type == NoteType::Normal)
        .map(|n| n.id.as_str())
        .collect();
    let mut endnotes: Vec<StoryPayload> = target
        .endnotes
        .iter()
        .filter(|n| n.note_type == NoteType::Normal)
        .map(|n| StoryPayload {
            id: n.id.clone(),
            segments: story_blocks_to_segments(&n.blocks),
        })
        .collect();
    endnotes.extend(
        base.endnotes
            .iter()
            .filter(|n| n.note_type == NoteType::Normal && !target_en_ids.contains(n.id.as_str()))
            .map(|n| StoryPayload {
                id: n.id.clone(),
                segments: story_blocks_to_segments(&n.blocks),
            }),
    );

    let target_comment_ids: HashSet<&str> = target.comments.iter().map(|c| c.id.as_str()).collect();
    let mut comments: Vec<CommentPayload> = target
        .comments
        .iter()
        .map(|c| {
            let (resolved, parent_para_id) =
                crate::domain::comment_extended_state(c, &target.comments_extended);
            CommentPayload {
                id: c.id.clone(),
                author: c.author.clone(),
                date: c.date.clone(),
                segments: story_blocks_to_segments(&c.blocks),
                resolved,
                parent_para_id,
            }
        })
        .collect();
    comments.extend(
        base.comments
            .iter()
            .filter(|c| !target_comment_ids.contains(c.id.as_str()))
            .map(|c| {
                let (resolved, parent_para_id) =
                    crate::domain::comment_extended_state(c, &base.comments_extended);
                CommentPayload {
                    id: c.id.clone(),
                    author: c.author.clone(),
                    date: c.date.clone(),
                    segments: story_blocks_to_segments(&c.blocks),
                    resolved,
                    parent_para_id,
                }
            }),
    );

    FullDocViewResult {
        blocks,
        footnotes,
        endnotes,
        comments,
        // Headers/footers follow the target section's bindings (target takes
        // precedence, matching footnotes/comments above).
        headers: crate::diff::project_section_headers(target),
        footers: crate::diff::project_section_footers(target),
        body_section_properties: target.body_section_properties.clone(),
    }
}
/// Convert story blocks (footnote/endnote/comment) to a flat list of inline segments.
pub(crate) fn story_blocks_to_segments(blocks: &[TrackedBlock]) -> Vec<InlineChange> {
    let mut segments = Vec::new();
    for (i, tracked_block) in blocks.iter().enumerate() {
        if i > 0 {
            // Separate paragraphs with a newline.
            segments.push(InlineChange::Unchanged {
                text: "\n".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            });
        }
        if let BlockNode::Paragraph(p) = &tracked_block.block {
            for inline in p.all_inlines() {
                match inline {
                    InlineNode::Text(t) => {
                        segments.push(InlineChange::Unchanged {
                            text: t.text.clone(),
                            marks: t.marks.clone(),
                            style_props: t.style_props.clone(),
                            formatting_change: t.formatting_change.clone(),
                        });
                    }
                    InlineNode::HardBreak(_) => {
                        segments.push(InlineChange::Unchanged {
                            text: "\n".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                        });
                    }
                    InlineNode::OpaqueInline(o) => {
                        let (text, reference_id, field_kind, field_instruction, asset_ref) =
                            match &o.kind {
                                OpaqueKind::Hyperlink(data) => {
                                    let text = if data.text.is_empty() {
                                        None
                                    } else {
                                        Some(data.text.clone())
                                    };
                                    // Surface URL (or #anchor for internal links) via asset_ref;
                                    // json_types.rs maps this to the `url` JSON field for hyperlinks.
                                    let url = data
                                        .url
                                        .clone()
                                        .or_else(|| data.anchor.as_ref().map(|a| format!("#{a}")));
                                    (text, None, None, None, url)
                                }
                                OpaqueKind::FootnoteReference(ref_data)
                                | OpaqueKind::EndnoteReference(ref_data)
                                | OpaqueKind::CommentReference(ref_data) => {
                                    (None, Some(ref_data.reference_id.clone()), None, None, None)
                                }
                                OpaqueKind::Field(field_data) => {
                                    // Prefer canonical instruction text from
                                    // the typed semantic (whitespace-invariant);
                                    // fall back to raw fragment bytes when no
                                    // semantic is parsed.
                                    let field_instruction = field_data
                                        .semantic
                                        .as_ref()
                                        .map(|s| s.to_instruction_text())
                                        .or_else(|| field_data.instruction_text.clone());
                                    (
                                        field_data.result_text.clone(),
                                        None,
                                        Some(field_data.field_kind.clone()),
                                        field_instruction,
                                        None,
                                    )
                                }
                                _ => (None, None, None, None, None),
                            };
                        segments.push(InlineChange::Opaque {
                            segment_type: InlineChangeSegmentType::Equal,
                            kind: crate::diff::opaque_kind_to_segment_kind(&o.kind),
                            opaque_id: o.id.0.to_string(),
                            inline_index: 0,
                            text,
                            reference_id,
                            field_kind,
                            field_instruction,
                            asset_ref,
                            asset_width_emu: None,
                            asset_height_emu: None,
                            alt_text: None,
                            url: crate::diff::opaque_url(&o.kind),
                            content_hash: o.content_hash.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    segments
}

/// Project a header/footer story into per-paragraph bands, keeping each `w:p`'s
/// alignment and tab stops (which `story_blocks_to_segments` flattens away). The
/// inline content reuses the same walker on a one-paragraph slice — for a single
/// block it emits no inter-paragraph separator, so it yields exactly that
/// paragraph's segments.
pub(crate) fn story_blocks_to_paragraphs(
    blocks: &[TrackedBlock],
) -> Vec<crate::domain::HeaderFooterParagraph> {
    blocks
        .iter()
        .filter_map(|tracked_block| match &tracked_block.block {
            BlockNode::Paragraph(p) => Some(crate::domain::HeaderFooterParagraph {
                align: p.align.clone(),
                tab_stops: p.tab_stops.clone(),
                segments: story_blocks_to_segments(std::slice::from_ref(tracked_block)),
            }),
            _ => None,
        })
        .collect()
}

/// Extract symbol character data from a `w:sym` element's raw XML.
///
/// Per ECMA-376 §17.3.3.30, the `char` attribute is a hex codepoint from the font
/// specified by `font`. The codepoint may be shifted into the Unicode Private Use Area
/// (U+F000..U+F0FF) for legacy font compatibility — we strip the F000 offset to get
/// the actual character position in the font.
fn extract_sym_data(raw_xml: &[u8]) -> Result<SymData, RuntimeError> {
    let el = crate::word_xml::parse_raw_fragment(raw_xml).map_err(|e| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!(
            "Failed to parse sym element XML: {e}: {}",
            String::from_utf8_lossy(raw_xml)
        ),
        details: ErrorDetails::default(),
    })?;

    let font = attr_get(&el, "w:font")
        .or_else(|| attr_get(&el, "font"))
        .cloned()
        .ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!(
                "sym element missing w:font attribute: {}",
                String::from_utf8_lossy(raw_xml)
            ),
            details: ErrorDetails::default(),
        })?;

    let char_code = attr_get(&el, "w:char")
        .or_else(|| attr_get(&el, "char"))
        .cloned()
        .ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!(
                "sym element missing w:char attribute: {}",
                String::from_utf8_lossy(raw_xml)
            ),
            details: ErrorDetails::default(),
        })?;

    let codepoint = u32::from_str_radix(&char_code, 16).map_err(|e| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!(
            "sym element has invalid hex char code '{char_code}': {e}: {}",
            String::from_utf8_lossy(raw_xml)
        ),
        details: ErrorDetails::default(),
    })?;

    // Strip the F000 PUA offset if present (§17.3.3.30: legacy fonts shift by F000)
    let actual_codepoint = if (0xF000..=0xF0FF).contains(&codepoint) {
        codepoint - 0xF000
    } else {
        codepoint
    };

    let display_char = char::from_u32(actual_codepoint).ok_or_else(|| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!(
            "sym element char code '{char_code}' (decoded {actual_codepoint:#06X}) is not a valid Unicode codepoint: {}",
            String::from_utf8_lossy(raw_xml)
        ),
        details: ErrorDetails::default(),
    })?;

    Ok(SymData {
        font,
        char_code,
        display_char,
    })
}

/// Extract `w:id` from a footnoteReference/endnoteReference/commentReference element's raw XML.
///
/// Returns an error if the XML cannot be parsed or doesn't contain a `w:id`/`id` attribute,
/// since a note reference without an ID is an invariant violation.
fn extract_note_reference_data(raw_xml: &[u8]) -> Result<NoteReferenceData, RuntimeError> {
    let el = crate::word_xml::parse_raw_fragment(raw_xml).map_err(|e| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!(
            "Failed to parse note reference XML: {e}: {}",
            String::from_utf8_lossy(raw_xml)
        ),
        details: ErrorDetails::default(),
    })?;
    let reference_id = attr_get(&el, "w:id")
        .or_else(|| attr_get(&el, "id"))
        .cloned()
        .ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!(
                "Note reference element missing w:id attribute: {}",
                String::from_utf8_lossy(raw_xml)
            ),
            details: ErrorDetails::default(),
        })?;
    Ok(NoteReferenceData { reference_id })
}
/// Extract field metadata from a fldChar/instrText/delInstrText/fldSimple element's raw XML.
///
/// Returns an error if the XML cannot be parsed or on unknown local names. An
/// `fldCharType` outside the begin|separate|end value domain (§17.18.29) is NOT an
/// error: it is modeled explicitly as `FieldKind::Unknown(raw)` and preserved
/// verbatim via the opaque anchor's `raw_xml`, matching Word's tolerant consumption.
fn extract_field_data(local_name: &str, raw_xml: &[u8]) -> Result<FieldData, RuntimeError> {
    let el = crate::word_xml::parse_raw_fragment(raw_xml).map_err(|e| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!(
            "Failed to parse field element '{local_name}' XML: {e}: {}",
            String::from_utf8_lossy(raw_xml)
        ),
        details: ErrorDetails::default(),
    })?;

    match local_name {
        "fldChar" => {
            let field_kind_str = attr_get(&el, "w:fldCharType")
                .or_else(|| attr_get(&el, "fldCharType"))
                .ok_or_else(|| RuntimeError {
                    code: ErrorCode::InvalidDocx,
                    message: format!(
                        "fldChar element missing fldCharType attribute: {}",
                        String::from_utf8_lossy(raw_xml)
                    ),
                    details: ErrorDetails::default(),
                })?;
            let field_kind = match field_kind_str.as_str() {
                "begin" => FieldKind::Begin,
                "separate" => FieldKind::Separate,
                "end" => FieldKind::End,
                // An fldCharType outside the ST_FldCharType value domain
                // (begin|separate|end, §17.18.29). Word opens such a document
                // clean (confirmed against real Word), so we do NOT refuse the file. This
                // is NOT a silent fallback: we represent the unknown type
                // EXPLICITLY (carrying the raw string), preserve the fldChar
                // byte-verbatim via the opaque anchor's raw_xml, and never treat
                // it as a begin/separate/end boundary downstream.
                other => FieldKind::Unknown(other.to_string()),
            };
            Ok(FieldData {
                field_kind,
                instruction_text: None,
                result_text: None,
                semantic: None,
            })
        }
        "instrText" | "delInstrText" => {
            let instruction_text = extract_element_text(&el);
            let semantic = parse_field_semantic(local_name, instruction_text.as_deref())?;
            Ok(FieldData {
                field_kind: FieldKind::Instruction,
                instruction_text,
                result_text: None,
                semantic,
            })
        }
        "fldSimple" => {
            let instruction_text = attr_get(&el, "w:instr")
                .or_else(|| attr_get(&el, "instr"))
                .cloned();
            let semantic = parse_field_semantic(local_name, instruction_text.as_deref())?;
            let result_text = {
                let text = extract_nested_run_text(&el);
                if text.is_empty() { None } else { Some(text) }
            };
            Ok(FieldData {
                field_kind: FieldKind::Simple,
                instruction_text,
                result_text,
                semantic,
            })
        }
        other => Err(RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!(
                "extract_field_data called with unexpected element name '{other}': {}",
                String::from_utf8_lossy(raw_xml)
            ),
            details: ErrorDetails::default(),
        }),
    }
}
/// Parse a field instruction string into a typed `FieldSemantic`. Empty
/// instructions and unknown field names produce `Ok(None)` / `Ok(Some(Other))`.
///
/// `<w:fldSimple w:instr="...">` is always a complete instruction, so a parse
/// failure there is treated as a hard error (no-fallback rule). `<w:instrText>`
/// and `<w:delInstrText>` carry only a *fragment* of an instruction when a
/// field is split across multiple runs (common with nested IF/MERGEFIELD); a
/// fragment that cannot stand alone returns `Ok(None)` rather than failing.
/// Multi-run instruction reassembly is a separate, deferred feature.
fn parse_field_semantic(
    local_name: &str,
    instruction_text: Option<&str>,
) -> Result<Option<crate::domain::FieldSemantic>, RuntimeError> {
    let Some(text) = instruction_text else {
        return Ok(None);
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let is_complete_instruction = local_name == "fldSimple";
    match crate::domain::parse_field_instruction(trimmed) {
        Ok(semantic) => Ok(Some(semantic)),
        Err(err) if is_complete_instruction => Err(RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!("Failed to parse fldSimple instruction text: {err}: {text:?}"),
            details: ErrorDetails::default(),
        }),
        Err(_) => Ok(None),
    }
}

/// Extract direct text content from an XML element (e.g., instrText).
fn extract_element_text(el: &Element) -> Option<String> {
    let mut text = String::new();
    for child in &el.children {
        if let XMLNode::Text(t) = child {
            text.push_str(t);
        }
    }
    if text.is_empty() { None } else { Some(text) }
}

/// Extract text from nested w:r/w:t elements (e.g., fldSimple result).
fn extract_nested_run_text(el: &Element) -> String {
    let mut text = String::new();
    for child in &el.children {
        if let XMLNode::Element(child_el) = child {
            let local = local_element_name(child_el);
            if local == "r" {
                // Look for w:t inside the run
                for rc in &child_el.children {
                    if let XMLNode::Element(rc_el) = rc
                        && local_element_name(rc_el) == "t"
                    {
                        for tc in &rc_el.children {
                            if let XMLNode::Text(t) = tc {
                                text.push_str(t);
                            }
                        }
                    }
                }
            } else if local == "t" {
                for tc in &child_el.children {
                    if let XMLNode::Text(t) = tc {
                        text.push_str(t);
                    }
                }
            }
        }
    }
    text
}
fn decoration_type_from_name(name: &str) -> DecorationType {
    // Extract local name, stripping any namespace prefix
    let local = if let Some(pos) = name.find(':') {
        &name[pos + 1..]
    } else {
        name
    };

    match local {
        s if s.starts_with("bookmark") => DecorationType::Bookmark,
        s if s.starts_with("commentRange") => DecorationType::CommentRange,
        s if s.starts_with("perm") => DecorationType::PermissionRange,
        "proofErr" => DecorationType::ProofError,
        s if s.starts_with("customXml") => DecorationType::CustomXmlRange,
        s if s.starts_with("move") => DecorationType::MoveRange,
        // Anything else reaching here is a FOREIGN-namespace element that
        // word_ir preserved verbatim as a zero-width decoration (e.g. a
        // PowerTools/Templafy <Insert> placeholder). Label it honestly rather
        // than mislabeling foreign markup as a WML bookmark — the raw_xml on the
        // DecorationNode carries the bytes that actually round-trip.
        _ => DecorationType::ForeignElement,
    }
}

/// True for run-level decorations whose isolated run's `w:rPr` is SEMANTICALLY
/// load-bearing — Word renders a glyph in that character formatting: the note
/// auto-number (`w:footnoteRef` §17.11.6 / `w:endnoteRef` §17.11.1), the note
/// separators (`w:separator` §17.11.11 / `w:continuationSeparator` §17.11.12),
/// and the annotation reference mark (`w:annotationRef` §17.11.13). These marks
/// each own their run in the note/comment story, so preserving the run's rPr is
/// exact-fidelity, not duplication.
///
/// Deliberately EXCLUDES the other `is_run_decoration` members
/// (`w:lastRenderedPageBreak` §17.3.3.16, a non-rendered pagination hint; and
/// `w:softHyphen` §17.3.3.29, a line-break opportunity). Those share a run with
/// adjacent text; the atom model splits each into its own run, so re-emitting
/// the host run's rPr onto the split marker run would merely duplicate the
/// formatting the neighbouring text run already carries — churn, not fidelity.
fn decoration_wrapper_rpr_is_load_bearing(name: &str) -> bool {
    let local = name.rsplit(':').next().unwrap_or(name);
    matches!(
        local,
        "footnoteRef" | "endnoteRef" | "separator" | "continuationSeparator" | "annotationRef"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::DocxFile;
    use crate::runtime::ErrorCode;

    /// Pre-Rung-6 whole-tree archive build: materialize the entire document.xml
    /// tree up front, parse header/footer refs from it, then run the body loop.
    /// Kept ONLY in tests as the oracle the streaming `build_canonical_from_archive`
    /// must reproduce CanonDoc-for-CanonDoc.
    fn reference_build_from_archive_whole_tree(
        archive: &DocxArchive,
        fingerprint: DocFingerprint,
    ) -> Result<(CanonDoc, Vec<Diagnostic>), RuntimeError> {
        let main_part =
            crate::docx_package::resolve_main_document_part(archive).map_err(map_package_error)?;
        let main_dir = crate::docx_package::part_dir(&main_part);
        let document_xml = archive
            .get(&main_part)
            .ok_or_else(|| invalid_docx(&format!("main document part {main_part} is missing")))?;
        let root = word_xml::parse_document_xml(document_xml).map_err(map_word_xml_error)?;

        let numbering_defs = parse_optional_docx_part(
            archive,
            "word/numbering.xml",
            crate::numbering::NumberingDefinitions::parse,
        )?;
        let mut style_defs = parse_optional_docx_part(
            archive,
            "word/styles.xml",
            crate::styles::StyleDefinitions::parse,
        )?;
        let theme_fonts = parse_optional_docx_part(
            archive,
            "word/theme/theme1.xml",
            crate::styles::ThemeFonts::parse,
        )?;
        if let (Some(theme_fonts), Some(ref mut sd)) = (theme_fonts, style_defs.as_mut()) {
            sd.set_theme_fonts(theme_fonts);
        }
        let default_tab_stop = crate::settings::parse_default_tab_stop(archive)
            .map_err(invalid_docx_message)?
            .unwrap_or(720);
        let compat_settings =
            crate::settings::parse_compat_settings(archive).map_err(invalid_docx_message)?;
        let even_and_odd_headers = crate::settings::parse_even_and_odd_headers_state(archive)
            .map_err(invalid_docx_message)?;
        let rels = parse_document_relationships(archive, &main_part)?;
        let (header_refs, footer_refs) = parse_header_footer_refs(&root)?;
        let mut story_diagnostics = Vec::new();
        let headers = parse_headers(
            archive,
            &rels,
            &header_refs,
            numbering_defs.as_ref(),
            style_defs.as_ref(),
            default_tab_stop,
            main_dir,
            &mut story_diagnostics,
        )?;
        let footers = parse_footers(
            archive,
            &rels,
            &footer_refs,
            numbering_defs.as_ref(),
            style_defs.as_ref(),
            default_tab_stop,
            main_dir,
            &mut story_diagnostics,
        )?;
        let footnotes = parse_footnotes(
            archive,
            &rels,
            numbering_defs.as_ref(),
            style_defs.as_ref(),
            default_tab_stop,
            main_dir,
        )?;
        let endnotes = parse_endnotes(
            archive,
            &rels,
            numbering_defs.as_ref(),
            style_defs.as_ref(),
            default_tab_stop,
            main_dir,
        )?;
        let comments = parse_comments(
            archive,
            &rels,
            numbering_defs.as_ref(),
            style_defs.as_ref(),
            default_tab_stop,
            main_dir,
        )?;
        let comments_extended = parse_comments_extended(archive, &rels, main_dir)?;
        let rel_lookup = build_rel_lookup_from_rels(&rels);

        let (mut doc, mut diagnostics) = build_canonical_from_root_with_stories(
            &root,
            fingerprint,
            numbering_defs.as_ref(),
            style_defs.as_ref(),
            default_tab_stop,
            &compat_settings,
            &rel_lookup,
            headers,
            footers,
            footnotes,
            endnotes,
            comments,
        )?;
        diagnostics.extend(story_diagnostics);
        doc.compat_settings = compat_settings;
        doc.comments_extended = comments_extended;
        doc.even_and_odd_headers = even_and_odd_headers;
        resolve_hyperlink_urls(&mut doc, &rels.hyperlinks);
        Ok((doc, diagnostics))
    }

    /// The streaming archive builder must produce the SAME CanonDoc as the
    /// whole-tree reference for every corpus document, on BOTH the normalized
    /// (accept-all) and tracked-preserving paths. This is the Rung-6 secondary
    /// parity gate. Set `STEMMA_CORPUS_ROOT` and run with `--ignored`.
    #[test]
    #[ignore = "corpus sweep; set STEMMA_CORPUS_ROOT — verifies streaming body build == whole-tree"]
    fn streaming_body_build_matches_whole_tree_on_corpus() {
        let root_dir = match std::env::var("STEMMA_CORPUS_ROOT") {
            Ok(r) => r,
            Err(_) => {
                eprintln!("STEMMA_CORPUS_ROOT not set; skipping");
                return;
            }
        };

        fn collect_docx(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_docx(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("docx") {
                    out.push(path);
                }
            }
        }

        let mut docs = Vec::new();
        collect_docx(std::path::Path::new(&root_dir), &mut docs);
        docs.sort();
        assert!(!docs.is_empty(), "no .docx found under {root_dir}");

        let limit = std::env::var("STEMMA_BODY_PARITY_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2000);
        if limit > 0 && docs.len() > limit {
            let stride = docs.len() / limit;
            docs = docs
                .into_iter()
                .enumerate()
                .filter(|(i, _)| i % stride == 0)
                .map(|(_, p)| p)
                .take(limit)
                .collect();
        }

        let mut checked = 0usize;
        let mut mismatches = Vec::new();
        for path in &docs {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let Ok(archive) = DocxArchive::read(&bytes) else {
                continue;
            };
            if ensure_docx_not_encrypted(&archive).is_err() {
                continue;
            }

            // Tracked-preserving path (no normalization).
            let fp = DocFingerprint("parity".to_string());
            let stream = build_canonical_from_archive(&archive, fp.clone());
            let reference = reference_build_from_archive_whole_tree(&archive, fp.clone());
            match (stream, reference) {
                (Ok((s, _)), Ok((r, _))) => {
                    if s != r {
                        mismatches.push(format!("{} (tracked): CanonDoc differs", path.display()));
                    }
                }
                (Err(_), Err(_)) => {}
                (s, r) => mismatches.push(format!(
                    "{} (tracked): result kind differs (stream_ok={}, ref_ok={})",
                    path.display(),
                    s.is_ok(),
                    r.is_ok()
                )),
            }

            // Accept-all (normalized) path.
            if let Ok(norm) = crate::normalize::normalize_if_needed(&archive) {
                let stream = build_canonical_from_archive(&norm, fp.clone());
                let reference = reference_build_from_archive_whole_tree(&norm, fp.clone());
                if let (Ok((s, _)), Ok((r, _))) = (stream, reference)
                    && s != r
                {
                    mismatches.push(format!("{} (normalized): CanonDoc differs", path.display()));
                }
            }

            checked += 1;
        }

        assert!(
            mismatches.is_empty(),
            "{} mismatch(es) across {checked} docs:\n{}",
            mismatches.len(),
            mismatches
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        eprintln!("streaming body build parity OK across {checked} corpus docs");
    }

    // =========================================================================
    // build_image_data_lookup tests
    // =========================================================================

    #[test]
    fn build_image_data_lookup_malformed_rels_xml_returns_error() {
        // A .rels file with invalid XML must produce an error, not silently
        // return an empty map.
        let archive = DocxArchive::from_parts(vec![DocxFile {
            name: "word/_rels/document.xml.rels".to_string(),
            data: b"<<< this is not valid XML >>>".to_vec(),
        }]);

        let result = build_image_data_lookup(&archive);
        let err = result.expect_err("malformed rels XML should return an error");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("word/_rels/document.xml.rels"),
            "error should name the failing file, got: {}",
            err.message
        );
    }

    #[test]
    fn build_image_data_lookup_missing_id_attribute_returns_error() {
        // A Relationship element without the required Id attribute must error.
        let rels_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;

        let archive = DocxArchive::from_parts(vec![DocxFile {
            name: "word/_rels/document.xml.rels".to_string(),
            data: rels_xml.to_vec(),
        }]);

        let err = build_image_data_lookup(&archive)
            .expect_err("missing Id attribute should return an error");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("Id"),
            "error should mention the missing attribute, got: {}",
            err.message
        );
    }

    #[test]
    fn build_image_data_lookup_missing_type_attribute_returns_error() {
        // A Relationship element without the required Type attribute must error.
        let rels_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Target="media/image1.png"/>
</Relationships>"#;

        let archive = DocxArchive::from_parts(vec![DocxFile {
            name: "word/_rels/document.xml.rels".to_string(),
            data: rels_xml.to_vec(),
        }]);

        let err = build_image_data_lookup(&archive)
            .expect_err("missing Type attribute should return an error");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("Type"),
            "error should mention the missing attribute, got: {}",
            err.message
        );
    }

    #[test]
    fn build_image_data_lookup_missing_target_attribute_returns_error() {
        // A Relationship element without the required Target attribute must error.
        let rels_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"/>
</Relationships>"#;

        let archive = DocxArchive::from_parts(vec![DocxFile {
            name: "word/_rels/document.xml.rels".to_string(),
            data: rels_xml.to_vec(),
        }]);

        let err = build_image_data_lookup(&archive)
            .expect_err("missing Target attribute should return an error");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("Target"),
            "error should mention the missing attribute, got: {}",
            err.message
        );
    }

    #[test]
    fn build_image_data_lookup_valid_rels_returns_image_map() {
        // Valid rels with an image relationship should produce a lookup entry
        // when the referenced image file exists in the archive.
        let rels_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;

        // 1x1 transparent PNG (minimal valid PNG)
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, // 8-bit RGB
        ];

        let archive = DocxArchive::from_parts(vec![
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: rels_xml.to_vec(),
            },
            DocxFile {
                name: "word/media/image1.png".to_string(),
                data: png_bytes,
            },
        ]);

        let lookup = build_image_data_lookup(&archive).expect("valid rels should succeed");
        assert_eq!(lookup.len(), 1, "should have exactly one image entry");
        assert!(
            lookup.contains_key("rId2"),
            "lookup should contain the image relationship ID"
        );
        let data_uri = &lookup["rId2"];
        assert!(
            data_uri.starts_with("data:image/png;base64,"),
            "data URI should have correct MIME type prefix, got: {}",
            &data_uri[..40.min(data_uri.len())]
        );
    }

    #[test]
    fn build_image_data_lookup_resolves_package_root_media_via_parent_target() {
        // Wild shape: media stored at the PACKAGE ROOT (media/image1.png), so the
        // image relationship in word/_rels/*.rels targets it as "../media/...".
        // OPC pack-URI resolution collapses the ".." against the rels base
        // directory (word/), landing on media/image1.png — the preview lookup
        // must find it rather than probing the non-existent word/../media/....
        let rels_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/>
</Relationships>"#;

        // 1x1 RGB PNG (minimal valid PNG header).
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE,
        ];

        let archive = DocxArchive::from_parts(vec![
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: rels_xml.to_vec(),
            },
            // The image part lives at the package root, NOT under word/media/.
            DocxFile {
                name: "media/image1.png".to_string(),
                data: png_bytes,
            },
        ]);

        let lookup = build_image_data_lookup(&archive)
            .expect("package-root media referenced via ../media must resolve");
        assert_eq!(lookup.len(), 1, "the package-root image must be found");
        assert!(
            lookup["rId7"].starts_with("data:image/png;base64,"),
            "resolved image must produce a PNG data URI, got: {}",
            &lookup["rId7"][..40.min(lookup["rId7"].len())]
        );
    }

    #[test]
    fn build_image_data_lookup_no_rels_files_returns_empty_map() {
        // An archive with no word/_rels/ files should return an empty map
        // (no rels to parse means no images — this is a valid state).
        let archive = DocxArchive::from_parts(vec![]);

        let lookup = build_image_data_lookup(&archive)
            .expect("empty archive should succeed with empty result");
        assert!(lookup.is_empty());
    }

    #[test]
    fn build_image_data_lookup_non_image_rels_skipped() {
        // Relationship elements with non-image Type should be skipped without
        // error (they are valid OPC, just not image relationships).
        let rels_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#;

        let archive = DocxArchive::from_parts(vec![DocxFile {
            name: "word/_rels/document.xml.rels".to_string(),
            data: rels_xml.to_vec(),
        }]);

        let lookup = build_image_data_lookup(&archive)
            .expect("non-image rels should succeed with empty result");
        assert!(lookup.is_empty());
    }

    // =========================================================================
    // parse_document_relationships tests (confirm existing error behavior)
    // =========================================================================

    #[test]
    fn parse_document_relationships_missing_rels_file_returns_error() {
        let archive = DocxArchive::from_parts(vec![]);

        let err = parse_document_relationships(&archive, "word/document.xml")
            .expect_err("missing rels file should return an error");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("word/_rels/document.xml.rels"),
            "error should name the expected file, got: {}",
            err.message
        );
    }

    #[test]
    fn parse_document_relationships_malformed_xml_returns_error() {
        let archive = DocxArchive::from_parts(vec![DocxFile {
            name: "word/_rels/document.xml.rels".to_string(),
            data: b"not xml at all".to_vec(),
        }]);

        let err = parse_document_relationships(&archive, "word/document.xml")
            .expect_err("malformed XML should return an error");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("word/_rels/document.xml.rels"),
            "error should name the failing file, got: {}",
            err.message
        );
    }

    // =========================================================================
    // parse_header_footer_ref tests
    // =========================================================================

    /// Helper: parse a headerReference XML fragment into an Element.
    fn header_ref_element(type_attr: &str) -> Element {
        use std::io::Cursor;
        let xml = format!(
            r#"<w:headerReference xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
                r:id="rId1" w:type="{}"/>"#,
            type_attr
        );
        Element::parse(Cursor::new(xml.as_bytes())).expect("test XML must parse")
    }

    #[test]
    fn parse_header_footer_ref_type_default() {
        let el = header_ref_element("default");
        let hf = parse_header_footer_ref(&el)
            .expect("should succeed")
            .expect("should return Some");
        assert_eq!(hf.kind, HeaderFooterKind::Default);
        assert_eq!(hf.rel_id, "rId1");
    }

    #[test]
    fn parse_header_footer_ref_type_first() {
        let el = header_ref_element("first");
        let hf = parse_header_footer_ref(&el)
            .expect("should succeed")
            .expect("should return Some");
        assert_eq!(hf.kind, HeaderFooterKind::First);
    }

    #[test]
    fn parse_header_footer_ref_type_even() {
        let el = header_ref_element("even");
        let hf = parse_header_footer_ref(&el)
            .expect("should succeed")
            .expect("should return Some");
        assert_eq!(hf.kind, HeaderFooterKind::Even);
    }

    #[test]
    fn parse_header_footer_ref_type_odd_maps_to_default() {
        // Apache POI and other producers emit w:type="odd" instead of "default".
        // "odd" is semantically equivalent: in OOXML, "default" already means
        // "odd pages" when evenAndOddHeaders is enabled.
        let el = header_ref_element("odd");
        let hf = parse_header_footer_ref(&el)
            .expect("should succeed")
            .expect("should return Some");
        assert_eq!(hf.kind, HeaderFooterKind::Default);
    }

    #[test]
    fn parse_header_footer_ref_unknown_type_defaults_to_default() {
        // §17.10.4 ST_HdrFtr only permits default/even/first, so an
        // unrecognized w:type is schema-invalid producer output. Refusing
        // the whole import over one unrecognized header/footer reference
        // would violate parse totality (invariant #1) — Word itself still
        // opens such a document. We degrade to Default like word_ir.rs's
        // sectPr headerReference/footerReference parsing does (unified via
        // parse_header_footer_kind), observably (tracing::warn) rather than
        // silently.
        let el = header_ref_element("bogus");
        let hf = parse_header_footer_ref(&el)
            .expect("unrecognized w:type must not refuse the import")
            .expect("should return Some");
        assert_eq!(hf.kind, HeaderFooterKind::Default);
    }

    // =========================================================================
    // parse_table_indent / parse_table_cell_spacing / parse_border_edge tests
    // =========================================================================

    fn tbl_pr_element(inner: &str) -> Element {
        use std::io::Cursor;
        let xml = format!(
            r#"<w:tblPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</w:tblPr>"#
        );
        Element::parse(Cursor::new(xml.as_bytes())).expect("test XML must parse")
    }

    #[test]
    fn parse_table_indent_plain_twips() {
        let el = tbl_pr_element(r#"<w:tblInd w:w="720" w:type="dxa"/>"#);
        assert_eq!(parse_table_indent(&el), Some(720));
    }

    /// A universal-measure `w:tblInd/@w` ("1in", schema-valid per
    /// ST_MeasurementOrPercent and Word-opens-clean, see
    /// `tblind_universal_measure_opens_clean` in
    /// spec_table_widths_grid_layout_word_compliance.rs) converts to twips.
    #[test]
    fn parse_table_indent_unit_suffixed_value_converts_to_twips() {
        let el = tbl_pr_element(r#"<w:tblInd w:w="1in" w:type="dxa"/>"#);
        assert_eq!(parse_table_indent(&el), Some(1440));
    }

    /// A percent tblInd has no twips meaning for a physical indent (Word
    /// ignores it) — dropped, not a parse failure. Invalid garbage is also
    /// dropped at this bounded, warned boundary.
    #[test]
    fn parse_table_indent_percent_and_garbage_are_dropped_not_erroring() {
        let el = tbl_pr_element(r#"<w:tblInd w:w="50%" w:type="dxa"/>"#);
        assert_eq!(parse_table_indent(&el), None);
        let el = tbl_pr_element(r#"<w:tblInd w:w="abc" w:type="dxa"/>"#);
        assert_eq!(parse_table_indent(&el), None);
    }

    #[test]
    fn parse_table_cell_spacing_plain_twips() {
        let el = tbl_pr_element(r#"<w:tblCellSpacing w:w="100" w:type="dxa"/>"#);
        assert_eq!(parse_table_cell_spacing(&el), Some(100));
    }

    /// A universal-measure `w:tblCellSpacing/@w` converts to twips
    /// (2cm = 2 * 1440/2.54 = 1134 twips, rounded).
    #[test]
    fn parse_table_cell_spacing_unit_suffixed_value_converts_to_twips() {
        let el = tbl_pr_element(r#"<w:tblCellSpacing w:w="2cm" w:type="dxa"/>"#);
        assert_eq!(parse_table_cell_spacing(&el), Some(1134));
    }

    /// Percent / garbage cell spacing is dropped, not a parse failure.
    #[test]
    fn parse_table_cell_spacing_percent_and_garbage_are_dropped_not_erroring() {
        let el = tbl_pr_element(r#"<w:tblCellSpacing w:w="50%" w:type="pct"/>"#);
        assert_eq!(parse_table_cell_spacing(&el), None);
        let el = tbl_pr_element(r#"<w:tblCellSpacing w:w="abc" w:type="dxa"/>"#);
        assert_eq!(parse_table_cell_spacing(&el), None);
    }

    fn borders_element(inner: &str) -> Element {
        use std::io::Cursor;
        let xml = format!(
            r#"<w:tblBorders xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</w:tblBorders>"#
        );
        Element::parse(Cursor::new(xml.as_bytes())).expect("test XML must parse")
    }

    #[test]
    fn parse_border_edge_plain_sz_and_space() {
        let el =
            borders_element(r#"<w:top w:val="single" w:sz="24" w:space="0" w:color="FF0000"/>"#);
        let border = parse_border_edge(&el, "top").unwrap().unwrap();
        assert_eq!(border.size, Some(24));
        assert_eq!(border.space, Some(0));
    }

    /// Unlike tblInd/tblCellSpacing, border `w:sz`/`w:space` are plain
    /// unbounded unsigned decimals (ST_EighthPointMeasure/ST_PointMeasure) —
    /// no universal-measure union ambiguity — so a malformed value really is
    /// invalid OOXML and fails fast, matching parse_table_measurement's
    /// w:tblW/w:tcW pattern.
    #[test]
    fn parse_border_edge_malformed_sz_fails_fast() {
        let el = borders_element(r#"<w:top w:val="single" w:sz="not-a-number" w:color="FF0000"/>"#);
        let err = parse_border_edge(&el, "top").expect_err("malformed w:sz must be a hard error");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
    }

    #[test]
    fn parse_border_edge_malformed_space_fails_fast() {
        let el = borders_element(r#"<w:top w:val="single" w:sz="24" w:space="not-a-number"/>"#);
        let err =
            parse_border_edge(&el, "top").expect_err("malformed w:space must be a hard error");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
    }

    #[test]
    fn parse_footers_keeps_referenced_even_footer_from_canada_fixture() {
        let bytes =
            std::fs::read("testdata/safe-us-vs-canada/after.docx").expect("read Canada after.docx");
        let archive = DocxArchive::read(&bytes).expect("open docx archive");
        let document_xml = archive
            .get("word/document.xml")
            .expect("document.xml must exist");
        let root = word_xml::parse_document_xml(document_xml).expect("parse document.xml");
        let rels = parse_document_relationships(&archive, "word/document.xml")
            .expect("parse relationships");
        let (_header_refs, footer_refs) =
            parse_header_footer_refs(&root).expect("parse footer refs");
        let footers = parse_footers(
            &archive,
            &rels,
            &footer_refs,
            None,
            None,
            720,
            "word/",
            &mut Vec::new(),
        )
        .expect("parse footer stories");

        let footer_kinds: Vec<(HeaderFooterKind, String)> = footers
            .iter()
            .map(|footer| (footer.kind.clone(), footer.part_name.clone()))
            .collect();
        assert!(
            footer_kinds
                .iter()
                .any(|(kind, part_name)| *kind == HeaderFooterKind::Even
                    && part_name == "footer1.xml"),
            "Canada fixture should keep the first-section even footer story; got {footer_kinds:?}"
        );
    }

    // =========================================================================
    // Story-part parse soundness (P0 #11): a part that is *present but
    // malformed* must fail loud, exactly like `parse_comments_extended`.
    // A *missing* part is legitimately empty (missing != malformed).
    // =========================================================================

    /// Build a `DocumentRelationships` referencing one story part by its
    /// conventional `word/<file>` path.
    fn rels_with_part(
        setter: impl FnOnce(&mut DocumentRelationships, Relationship),
    ) -> DocumentRelationships {
        let mut rels = DocumentRelationships::default();
        let rel = Relationship {
            id: "rId99".to_string(),
            target: "footnotes.xml".to_string(),
        };
        setter(&mut rels, rel);
        rels
    }

    fn archive_with_part(name: &str, data: &[u8]) -> DocxArchive {
        DocxArchive::from_parts(vec![DocxFile {
            name: name.to_string(),
            data: data.to_vec(),
        }])
    }

    const MALFORMED_XML: &[u8] = b"<w:footnotes><w:footnote> truncated, never closed";

    #[test]
    fn parse_footnotes_malformed_part_is_an_error_not_absent() {
        let rels = rels_with_part(|r, rel| r.footnotes = Some(rel));
        let archive = archive_with_part("word/footnotes.xml", MALFORMED_XML);
        let err = parse_footnotes(&archive, &rels, None, None, 720, "word/").expect_err(
            "a present-but-malformed footnotes.xml must fail loud, not import as no footnotes",
        );
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("footnotes.xml"),
            "error should name the failing part, got: {}",
            err.message
        );
    }

    #[test]
    fn parse_footnotes_missing_part_is_empty_not_error() {
        // The relationship target points at a part that is absent from the
        // archive: a document simply has no footnotes. That is the contract,
        // and must remain distinct from the malformed case above.
        let rels = rels_with_part(|r, rel| r.footnotes = Some(rel));
        let archive = DocxArchive::from_parts(vec![]);
        let footnotes = parse_footnotes(&archive, &rels, None, None, 720, "word/")
            .expect("a missing footnotes part is legitimately empty, not an error");
        assert!(footnotes.is_empty());
    }

    #[test]
    fn parse_endnotes_malformed_part_is_an_error_not_absent() {
        let rels = rels_with_part(|r, rel| r.endnotes = Some(rel));
        let archive = archive_with_part("word/footnotes.xml", MALFORMED_XML);
        let err = parse_endnotes(&archive, &rels, None, None, 720, "word/")
            .expect_err("a present-but-malformed endnotes part must fail loud");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
    }

    #[test]
    fn parse_comments_malformed_part_is_an_error_not_absent() {
        let rels = rels_with_part(|r, rel| r.comments = Some(rel));
        let archive = archive_with_part("word/footnotes.xml", MALFORMED_XML);
        let err = parse_comments(&archive, &rels, None, None, 720, "word/")
            .expect_err("a present-but-malformed comments part must fail loud");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
    }

    #[test]
    fn parse_headers_malformed_part_is_an_error_not_absent() {
        let mut rels = DocumentRelationships::default();
        rels.headers.push(Relationship {
            id: "rId10".to_string(),
            target: "header1.xml".to_string(),
        });
        let header_refs = vec![HeaderFooterRef {
            rel_id: "rId10".to_string(),
            kind: HeaderFooterKind::Default,
        }];
        let archive = archive_with_part("word/header1.xml", MALFORMED_XML);
        let err = parse_headers(
            &archive,
            &rels,
            &header_refs,
            None,
            None,
            720,
            "word/",
            &mut Vec::new(),
        )
        .expect_err("a present-but-malformed header part must fail loud");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("header1.xml"),
            "error should name the failing part, got: {}",
            err.message
        );
    }

    #[test]
    fn parse_footers_malformed_part_is_an_error_not_absent() {
        let mut rels = DocumentRelationships::default();
        rels.footers.push(Relationship {
            id: "rId11".to_string(),
            target: "footer1.xml".to_string(),
        });
        let footer_refs = vec![HeaderFooterRef {
            rel_id: "rId11".to_string(),
            kind: HeaderFooterKind::Default,
        }];
        let archive = archive_with_part("word/footer1.xml", MALFORMED_XML);
        let err = parse_footers(
            &archive,
            &rels,
            &footer_refs,
            None,
            None,
            720,
            "word/",
            &mut Vec::new(),
        )
        .expect_err("a present-but-malformed footer part must fail loud");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
    }

    // =========================================================================
    // Story-part tracked-segment preservation (P0 #9): a w:del/w:ins inside a
    // footnote/endnote/comment/header paragraph must import as a tracked segment,
    // not be flattened to Normal (which turned deleted text into live text).
    // =========================================================================

    #[test]
    fn story_paragraph_preserves_tracked_deletion_segments() {
        use std::io::Cursor;
        let xml = r#"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1">
            <w:p>
                <w:r><w:t xml:space="preserve">kept </w:t></w:r>
                <w:del w:id="5" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:delText>removed</w:delText></w:r></w:del>
            </w:p>
        </w:footnote>"#;
        let el = Element::parse(Cursor::new(xml.as_bytes())).expect("parse footnote element");
        let blocks = parse_note_blocks(&el, None, None, 720).expect("parse note blocks");
        let para = blocks
            .iter()
            .find_map(|b| match b {
                BlockNode::Paragraph(p) => Some(p),
                _ => None,
            })
            .expect("footnote should yield a paragraph");

        // The deleted run must be a Deleted tracked segment carrying "removed".
        let has_deleted_removed = para.segments.iter().any(|s| {
            matches!(s.status, TrackingStatus::Deleted(_))
                && s.inlines
                    .iter()
                    .any(|i| matches!(i, InlineNode::Text(t) if t.text.contains("removed")))
        });
        assert!(
            has_deleted_removed,
            "a footnote w:del must import as a Deleted tracked segment, got: {:?}",
            para.segments
                .iter()
                .map(|s| (&s.status, extract_inline_text_simple(&s.inlines)))
                .collect::<Vec<_>>()
        );

        // And the deleted text must NOT appear as live (Normal) text.
        let normal_has_removed = para.segments.iter().any(|s| {
            s.status == TrackingStatus::Normal
                && s.inlines
                    .iter()
                    .any(|i| matches!(i, InlineNode::Text(t) if t.text.contains("removed")))
        });
        assert!(
            !normal_has_removed,
            "deleted footnote text must not be flattened into a live Normal segment"
        );
    }

    // =========================================================================
    // Story-part block recovery (P0 #10): non-p/tbl story blocks must not be
    // silently dropped — wrappers are descended into.
    // =========================================================================

    fn footnote_para_texts(xml: &str) -> Vec<String> {
        use std::io::Cursor;
        let el = Element::parse(Cursor::new(xml.as_bytes())).expect("parse note element");
        let blocks = parse_note_blocks(&el, None, None, 720).expect("parse note blocks");
        blocks
            .iter()
            .filter_map(|b| match b {
                BlockNode::Paragraph(p) => {
                    Some(extract_block_text(&BlockNode::Paragraph(p.clone())))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn story_sdt_wrapped_paragraph_is_recovered_not_dropped() {
        // A w:sdt wrapping a paragraph inside a note (e.g. a header page-number
        // gallery) must have its paragraph recovered, not dropped.
        let xml = r#"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1">
            <w:sdt><w:sdtPr><w:id w:val="9"/></w:sdtPr><w:sdtContent>
                <w:p><w:r><w:t>inside the control</w:t></w:r></w:p>
            </w:sdtContent></w:sdt>
        </w:footnote>"#;
        let texts = footnote_para_texts(xml);
        assert!(
            texts.iter().any(|t| t.contains("inside the control")),
            "SDT-wrapped story paragraph must be recovered, got: {texts:?}"
        );
    }

    #[test]
    fn story_block_level_del_content_is_recovered_not_dropped() {
        // A body-level w:del wrapping a whole paragraph in a footnote must have
        // its content recovered (block-level story tracking is flattened with a
        // diagnostic, but the text is not silently lost).
        let xml = r#"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1">
            <w:del w:id="3" w:author="A" w:date="2024-01-01T00:00:00Z">
                <w:p><w:r><w:t>deleted whole paragraph</w:t></w:r></w:p>
            </w:del>
        </w:footnote>"#;
        let texts = footnote_para_texts(xml);
        assert!(
            texts.iter().any(|t| t.contains("deleted whole paragraph")),
            "block-level w:del paragraph content must be recovered, not dropped, got: {texts:?}"
        );
    }

    // =========================================================================
    // parse_twips tests
    // =========================================================================

    #[test]
    fn parse_twips_integer_value() {
        assert_eq!(parse_twips("9634", "test").unwrap(), 9634);
    }

    #[test]
    fn parse_twips_float_value_truncates_to_integer() {
        // Real-world producers sometimes emit "9634.0" instead of "9634".
        // Twips are integer units, so we truncate.
        assert_eq!(parse_twips("9634.0", "test").unwrap(), 9634);
    }

    #[test]
    fn parse_twips_float_with_fractional_part_truncates() {
        assert_eq!(parse_twips("9634.7", "test").unwrap(), 9634);
    }

    #[test]
    fn parse_twips_zero() {
        assert_eq!(parse_twips("0", "test").unwrap(), 0);
        assert_eq!(parse_twips("0.0", "test").unwrap(), 0);
    }

    #[test]
    fn parse_twips_non_numeric_returns_error() {
        let err = parse_twips("abc", "tblW element").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("abc") && err.message.contains("tblW element"),
            "error should include the bad value and context, got: {}",
            err.message
        );
    }

    #[test]
    fn parse_twips_negative_returns_error() {
        let err = parse_twips("-100", "tblW element").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidDocx);
    }

    // =========================================================================
    // parse_measurement_or_percent tests — ST_MeasurementOrPercent §17.18.107
    // =========================================================================

    #[test]
    fn parse_measurement_plain_number() {
        assert_eq!(
            parse_measurement_or_percent("5000", "test").unwrap(),
            MeasurementOrPercent::Number(5000)
        );
        // Tolerant float spelling, same policy as parse_twips.
        assert_eq!(
            parse_measurement_or_percent("9634.7", "test").unwrap(),
            MeasurementOrPercent::Number(9634)
        );
    }

    #[test]
    fn parse_measurement_percent_literals() {
        // §17.18.107: ST_Percentage admits -?[0-9]+(\.[0-9]+)?% — "100%" and
        // "40.0%" are legal values. 1 unit of the plain-number pct form is
        // one fiftieth of a percent, so 100% = 5000 and 40.0% = 2000.
        assert_eq!(
            parse_measurement_or_percent("100%", "tblW element").unwrap(),
            MeasurementOrPercent::Percent {
                fiftieths: 5000,
                literal: "100%".to_string()
            }
        );
        assert_eq!(
            parse_measurement_or_percent("40.0%", "tcW element").unwrap(),
            MeasurementOrPercent::Percent {
                fiftieths: 2000,
                literal: "40.0%".to_string()
            }
        );
        assert_eq!(
            parse_measurement_or_percent("33.3%", "tcW element").unwrap(),
            MeasurementOrPercent::Percent {
                fiftieths: 1665,
                literal: "33.3%".to_string()
            }
        );
    }

    #[test]
    fn parse_measurement_universal_measures() {
        // §22.9.2.15: 1in = 1440 twips, 1pt = 20 twips, 1pc = 240 twips,
        // 2.54cm = 1in.
        assert_eq!(
            parse_measurement_or_percent("1.5in", "tblW element").unwrap(),
            MeasurementOrPercent::UniversalTwips(2160)
        );
        assert_eq!(
            parse_measurement_or_percent("12pt", "test").unwrap(),
            MeasurementOrPercent::UniversalTwips(240)
        );
        assert_eq!(
            parse_measurement_or_percent("2.54cm", "test").unwrap(),
            MeasurementOrPercent::UniversalTwips(1440)
        );
    }

    #[test]
    fn parse_measurement_rejects_illegal_values() {
        // Fail-fast stays: values matching none of the three legal forms are
        // InvalidDocx errors, with the value and carrier in the message.
        for bad in ["abc", "%", "1.5.0%", "12xx", "--5%", "1e3%", "in"] {
            let err = parse_measurement_or_percent(bad, "tblW element").unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidDocx, "value: {bad}");
            assert!(
                err.message.contains(bad) && err.message.contains("tblW element"),
                "error should include the bad value and context, got: {}",
                err.message
            );
        }
    }

    #[test]
    fn parse_twips_measure_universal_and_plain() {
        // ST_TwipsMeasure (gridCol/@w, trHeight/@val) admits a positive
        // universal measure alongside plain twips.
        assert_eq!(parse_twips_measure("1in", "gridCol element").unwrap(), 1440);
        assert_eq!(parse_twips_measure("720", "gridCol element").unwrap(), 720);
        // Percent is NOT in ST_TwipsMeasure's value space — fail-fast.
        assert!(parse_twips_measure("50%", "gridCol element").is_err());
    }

    // =========================================================================
    // match_prefix_pattern tests — multi-level decimal support
    // =========================================================================

    #[test]
    fn match_prefix_single_level_with_tab() {
        // "4.\tSecurity" → ("4.", 3)
        let result = match_prefix_pattern("4.\tSecurity");
        assert_eq!(result, Some(("4.".into(), 3)));
    }

    #[test]
    fn match_prefix_single_level_with_space() {
        // "4. Security" → ("4.", 3)
        let result = match_prefix_pattern("4. Security");
        assert_eq!(result, Some(("4.".into(), 3)));
    }

    #[test]
    fn match_prefix_multi_level_two_parts() {
        // "4.3\tBreach Notification" → ("4.3", 4)
        let result = match_prefix_pattern("4.3\tBreach Notification");
        assert_eq!(result, Some(("4.3".into(), 4)));
    }

    #[test]
    fn match_prefix_multi_level_two_parts_space() {
        // "4.3 Breach" → ("4.3", 4)
        let result = match_prefix_pattern("4.3 Breach");
        assert_eq!(result, Some(("4.3".into(), 4)));
    }

    #[test]
    fn match_prefix_multi_level_with_trailing_period() {
        // "4.3.\tBreach" → ("4.3.", 5)
        let result = match_prefix_pattern("4.3.\tBreach");
        assert_eq!(result, Some(("4.3.".into(), 5)));
    }

    #[test]
    fn match_prefix_multi_level_three_parts() {
        // "10.2.3\tDetails" → ("10.2.3", 7)
        let result = match_prefix_pattern("10.2.3\tDetails");
        assert_eq!(result, Some(("10.2.3".into(), 7)));
    }

    #[test]
    fn match_prefix_multi_level_three_parts_trailing_period() {
        // "10.2.3.\tDetails" → ("10.2.3.", 8)
        let result = match_prefix_pattern("10.2.3.\tDetails");
        assert_eq!(result, Some(("10.2.3.".into(), 8)));
    }

    #[test]
    fn match_prefix_multi_level_no_separator() {
        // "4.3text" — no separator after digits → None
        let result = match_prefix_pattern("4.3text");
        assert_eq!(result, None);
    }

    #[test]
    fn match_prefix_paren() {
        // "(a)\tFirst item" → ("(a)", 4)
        let result = match_prefix_pattern("(a)\tFirst item");
        assert_eq!(result, Some(("(a)".into(), 4)));
    }

    #[test]
    fn strip_literal_prefix_accepts_parenthesized_label_without_explicit_separator_after_leading_tab()
     {
        let mut inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: "\t(g)Unless otherwise stated herein".to_string(),
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: RunRprAuthored::default(),
            formatting_change: None,
        })];

        let stripped = strip_literal_prefix(&mut inlines).expect("should strip literal prefix");
        assert_eq!(stripped.label, "(g)");
        assert_eq!(stripped.leading_tab_count, 1);
        assert!(!stripped.has_trailing_tab);

        let remaining = match &inlines[0] {
            InlineNode::Text(text) => text.text.as_str(),
            other => panic!("expected text inline, got {other:?}"),
        };
        assert_eq!(remaining, "Unless otherwise stated herein");
    }

    #[test]
    fn match_prefix_digit_paren() {
        // "1)\tFirst" → ("1)", 3)
        let result = match_prefix_pattern("1)\tFirst");
        assert_eq!(result, Some(("1)".into(), 3)));
    }

    #[test]
    fn match_prefix_no_match() {
        // "Hello world" → None
        assert_eq!(match_prefix_pattern("Hello world"), None);
    }

    #[test]
    fn match_prefix_empty() {
        assert_eq!(match_prefix_pattern(""), None);
    }

    fn minimal_docx_bytes_with_optional_part(
        optional_part_name: &str,
        optional_part_data: &[u8],
    ) -> Vec<u8> {
        let archive = DocxArchive::from_parts(vec![
            DocxFile {
                name: "_rels/.rels".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#
                    .to_vec(),
            },
            DocxFile {
                name: "word/document.xml".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Hello</w:t></w:r></w:p>
  </w:body>
</w:document>"#
                    .to_vec(),
            },
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#
                    .to_vec(),
            },
            DocxFile {
                name: optional_part_name.to_string(),
                data: optional_part_data.to_vec(),
            },
        ]);

        archive.write().expect("test DOCX archive should serialize")
    }

    #[test]
    fn build_canonical_from_docx_rejects_malformed_settings_xml() {
        let bytes = minimal_docx_bytes_with_optional_part("word/settings.xml", b"not xml");
        let err = build_canonical_from_docx(&bytes, DocFingerprint("fp".to_string()))
            .expect_err("malformed settings.xml must fail import");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(err.message.contains("word/settings.xml"));
    }

    #[test]
    fn build_canonical_from_docx_rejects_malformed_styles_xml() {
        let bytes = minimal_docx_bytes_with_optional_part("word/styles.xml", b"not xml");
        let err = build_canonical_from_docx(&bytes, DocFingerprint("fp".to_string()))
            .expect_err("malformed styles.xml must fail import");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(err.message.contains("word/styles.xml"));
    }

    #[test]
    fn build_canonical_from_docx_rejects_malformed_numbering_xml() {
        let bytes = minimal_docx_bytes_with_optional_part("word/numbering.xml", b"not xml");
        let err = build_canonical_from_docx(&bytes, DocFingerprint("fp".to_string()))
            .expect_err("malformed numbering.xml must fail import");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(err.message.contains("word/numbering.xml"));
    }

    #[test]
    fn build_canonical_from_docx_rejects_malformed_theme_xml() {
        let bytes = minimal_docx_bytes_with_optional_part("word/theme/theme1.xml", b"not xml");
        let err = build_canonical_from_docx(&bytes, DocFingerprint("fp".to_string()))
            .expect_err("malformed theme1.xml must fail import");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(err.message.contains("word/theme/theme1.xml"));
    }

    // =========================================================================
    // Compat tolerance for schema-INVALID but Word-ACCEPTED input shapes
    // (see `crate::compat`). Each shape has a real Word-opens-it witness; the
    // rule is: import succeeds, the tolerance is recorded as a diagnostic
    // (visibility), the mapped semantics hold, and the result round-trips
    // idempotently. A neighbouring non-witnessed invalid shape still fails fast.
    // =========================================================================

    /// Wrap a `w:body` inner XML fragment into a minimal single-part DOCX.
    fn docx_with_body(body_inner_xml: &str) -> Vec<u8> {
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>{body_inner_xml}</w:body>
</w:document>"#
        );
        let archive = DocxArchive::from_parts(vec![
            DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#
                    .to_vec(),
            },
            DocxFile {
                name: "_rels/.rels".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#
                    .to_vec(),
            },
            DocxFile {
                name: "word/document.xml".to_string(),
                data: document_xml.into_bytes(),
            },
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#
                    .to_vec(),
            },
        ]);
        archive.write().expect("test DOCX archive should serialize")
    }

    /// Import bytes and return (canonical, diagnostics) — the diagnostics-
    /// preserving path, so the tolerance's visibility contract is assertable.
    fn import_with_diagnostics(bytes: &[u8]) -> (CanonDoc, Vec<Diagnostic>) {
        build_canonical_from_docx_preserving_tracked(bytes, DocFingerprint("compat".to_string()))
            .expect("witnessed compat shape must import cleanly")
    }

    /// Assert one import → export → re-import → export cycle is a fixed point:
    /// after the first import normalizes the invalid shape, serialization is
    /// stable (2-cycle idempotence). Uses the validated export path, so the
    /// output also passes the Blocking structural validator Word relies on.
    fn assert_roundtrip_idempotent(bytes: &[u8]) {
        use crate::ExportOptions;
        use crate::api::{Document, validate};
        let doc1 = Document::parse(bytes).expect("parse");
        let bytes1 = doc1
            .serialize(&ExportOptions::default())
            .expect("first validated export");
        assert!(validate(&bytes1).ok, "first export must pass validation");
        let doc2 = Document::parse(&bytes1).expect("re-parse");
        let bytes2 = doc2
            .serialize(&ExportOptions::default())
            .expect("second validated export");
        assert_eq!(
            bytes1, bytes2,
            "export must be a fixed point (2-cycle idempotent)"
        );
    }

    // =========================================================================
    // Empty running-head parts (0-byte / whitespace-only header/footer parts).
    // Word emits these for an empty running head and opens them without repair,
    // so import must treat a REFERENCED empty part as an empty running head (no
    // blocks) with a visible diagnostic — not as a parse failure. A referenced
    // part that has content but no root is genuinely malformed and still fails
    // loud (the empty-vs-truncated boundary).
    // =========================================================================

    /// Build a minimal DOCX whose body section references a Default header part,
    /// with `header_bytes` as the literal contents of `word/header1.xml`.
    fn docx_with_default_header(header_bytes: &[u8]) -> Vec<u8> {
        let archive = DocxArchive::from_parts(vec![
            DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>
</Types>"#
                    .to_vec(),
            },
            DocxFile {
                name: "_rels/.rels".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#
                    .to_vec(),
            },
            DocxFile {
                name: "word/document.xml".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:t>Body</w:t></w:r></w:p>
    <w:sectPr><w:headerReference w:type="default" r:id="rIdH"/></w:sectPr>
  </w:body>
</w:document>"#
                    .to_vec(),
            },
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rIdH" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#
                    .to_vec(),
            },
            DocxFile {
                name: "word/header1.xml".to_string(),
                data: header_bytes.to_vec(),
            },
        ]);
        archive.write().expect("test DOCX archive should serialize")
    }

    /// The default (`kind == Default`) header story imported from a fixture.
    fn default_header(doc: &CanonDoc) -> &HeaderStory {
        doc.headers
            .iter()
            .find(|h| h.kind == HeaderFooterKind::Default)
            .expect("the fixture references a Default header")
    }

    #[test]
    fn import_treats_zero_byte_referenced_header_as_empty_running_head() {
        let bytes = docx_with_default_header(b"");
        let (doc, diagnostics) = import_with_diagnostics(&bytes);

        let header = default_header(&doc);
        assert!(
            header.blocks.is_empty(),
            "a 0-byte referenced header is an empty running head (no blocks)"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("tolerated empty running-head part")
                    && d.context.as_deref() == Some("word/header1.xml")),
            "the empty running head must be surfaced as a diagnostic, not absorbed"
        );
    }

    #[test]
    fn import_treats_whitespace_only_referenced_header_as_empty_running_head() {
        let bytes = docx_with_default_header(b"\r\n  \t\n");
        let (doc, _diagnostics) = import_with_diagnostics(&bytes);
        assert!(
            default_header(&doc).blocks.is_empty(),
            "a whitespace-only referenced header is an empty running head (no blocks)"
        );
    }

    #[test]
    fn create_header_succeeds_on_doc_with_empty_referenced_header() {
        use crate::ExportOptions;
        use crate::api::{Document, validate};
        use crate::domain::RevisionInfo;
        use crate::edit::{EditStep, EditTransaction, MaterializationMode};

        let bytes = docx_with_default_header(b"");
        let doc = Document::parse(&bytes).expect("import a doc with an empty Default header");

        // The Default kind is already referenced (empty), so author a net-new
        // Even running head — the genuinely-created case.
        let txn = EditTransaction {
            steps: vec![EditStep::CreateHeader {
                kind: HeaderFooterKind::Even,
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                author: Some("test".to_string()),
                date: None,
                apply_op_id: None,
            },
        };
        let edited = doc
            .apply(&txn)
            .expect("CreateHeader must succeed, not InvalidDocx");
        let out = edited
            .serialize(&ExportOptions::default())
            .expect("serialize must produce validated bytes");
        assert!(
            validate(&out).ok,
            "output must pass the structural validator"
        );
        Document::parse(&out).expect("output must re-import cleanly");
    }

    #[test]
    fn import_rejects_nonempty_rootless_header_part() {
        // Content present but no root element (a bare XML declaration): this is a
        // truncated/malformed part, NOT an empty running head — it must fail
        // loud, never be silently swallowed as empty.
        let bytes = docx_with_default_header(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        let err = build_canonical_from_docx(&bytes, DocFingerprint("fp".to_string()))
            .expect_err("a rootless-but-nonempty header part must fail import");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("word/header1.xml"),
            "the error must name the offending part, got: {}",
            err.message
        );
    }

    #[test]
    fn compat_tolerates_shd_val_none_and_maps_to_no_shading() {
        // ST_Shd (§17.18.78) has no "none"; Word renders it as no shading.
        let bytes = docx_with_body(
            r#"<w:p><w:pPr><w:shd w:val="none"/></w:pPr><w:r><w:t>Body</w:t></w:r></w:p>"#,
        );

        let (doc, diags) = import_with_diagnostics(&bytes);

        // Visibility: the tolerance is recorded.
        let shd_diag = diags
            .iter()
            .find(|d| d.message.contains("w:shd w:val=\"none\""))
            .expect("a diagnostic must record the tolerated w:shd w:val=\"none\"");
        assert_eq!(shd_diag.level, DiagnosticLevel::Info);

        // Mapped semantics: no shading on the paragraph.
        let para = doc
            .blocks
            .iter()
            .find_map(|tb| match &tb.block {
                BlockNode::Paragraph(p) => Some(p),
                _ => None,
            })
            .expect("one paragraph");
        assert!(
            para.shading.is_none(),
            "w:val=\"none\" must map to no shading, got {:?}",
            para.shading
        );

        assert_roundtrip_idempotent(&bytes);
    }

    #[test]
    fn compat_hoists_table_that_is_a_direct_child_of_paragraph() {
        // CT_P (§17.3.1.22) has no tbl child; Word renders the table as content.
        let bytes = docx_with_body(
            r#"<w:p><w:r><w:t>Intro</w:t></w:r><w:tbl>
                 <w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr>
                 <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
                 <w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="pct"/></w:tcPr>
                   <w:p><w:r><w:t>CellText</w:t></w:r></w:p>
                 </w:tc></w:tr>
               </w:tbl></w:p>"#,
        );

        let (doc, diags) = import_with_diagnostics(&bytes);

        let hoist_diag = diags
            .iter()
            .find(|d| d.message.contains("<w:tbl> as a direct child"))
            .expect("a diagnostic must record the tolerated tbl-in-p hoist");
        assert_eq!(hoist_diag.level, DiagnosticLevel::Warning);

        // Mapped semantics: the host paragraph, then the hoisted table sibling,
        // in document order. The paragraph keeps its own remaining content.
        let kinds: Vec<&str> = doc
            .blocks
            .iter()
            .map(|tb| match &tb.block {
                BlockNode::Paragraph(_) => "p",
                BlockNode::Table(_) => "tbl",
                _ => "other",
            })
            .collect();
        let p = kinds.iter().position(|k| *k == "p").expect("a paragraph");
        assert_eq!(
            kinds.get(p + 1).copied(),
            Some("tbl"),
            "table must be hoisted to the sibling immediately after its host paragraph; got {kinds:?}"
        );

        // Cell text survives the hoist.
        assert!(
            doc.blocks
                .iter()
                .any(|tb| matches!(&tb.block, BlockNode::Table(_)))
        );
        use crate::api::Document;
        assert!(
            Document::parse(&bytes)
                .unwrap()
                .to_text()
                .contains("CellText")
        );

        assert_roundtrip_idempotent(&bytes);
    }

    #[test]
    fn compat_flattens_nested_run_into_sibling_runs() {
        // CT_R (§17.3.2.25) has no r child; generators emit it in TOC/field runs.
        let bytes = docx_with_body(
            r#"<w:p><w:r>
                 <w:fldChar w:fldCharType="begin"/>
                 <w:instrText xml:space="preserve"> TOC \o "1-2" \h </w:instrText>
                 <w:fldChar w:fldCharType="separate"/>
                 <w:r><w:rPr><w:b/></w:rPr><w:t>Field result</w:t></w:r>
                 <w:fldChar w:fldCharType="end"/>
               </w:r></w:p>"#,
        );

        let (_doc, diags) = import_with_diagnostics(&bytes);

        let flatten_diag = diags
            .iter()
            .find(|d| d.message.contains("nested <w:r> inside <w:r>"))
            .expect("a diagnostic must record the tolerated nested-run flatten");
        assert_eq!(flatten_diag.level, DiagnosticLevel::Warning);

        // Mapped semantics: the inner run's text is preserved in the stream.
        use crate::api::Document;
        assert!(
            Document::parse(&bytes)
                .unwrap()
                .to_text()
                .contains("Field result"),
            "nested run content must survive flattening"
        );

        assert_roundtrip_idempotent(&bytes);
    }

    #[test]
    fn compat_still_fails_fast_on_non_witnessed_invalid_shading() {
        // A DIFFERENT unknown ST_Shd value has no Word-accepts contract, so the
        // fail-fast rule is preserved: it must NOT be tolerated.
        let bytes = docx_with_body(
            r#"<w:p><w:pPr><w:shd w:val="bogus"/></w:pPr><w:r><w:t>Body</w:t></w:r></w:p>"#,
        );
        let err = build_canonical_from_docx_preserving_tracked(
            &bytes,
            DocFingerprint("compat".to_string()),
        )
        .expect_err("an unknown, non-witnessed shading value must still fail fast");
        assert_eq!(err.code, ErrorCode::InvalidDocx);
        assert!(
            err.message.contains("ShadingPattern") && err.message.contains("bogus"),
            "error must name the offending value; got {}",
            err.message
        );
    }

    // =========================================================================
    // Story-part heading-level derivation: a heading is a heading regardless of
    // which story it lives in. A paragraph that resolves to a heading level in
    // the body (via direct outlineLvl §17.3.1.20, or a built-in Heading1–9
    // style id) must resolve to the SAME level inside a header/footer/footnote/
    // endnote/comment. The story importer previously hardcoded heading_level:
    // None, silently dropping the level — a divergence from the body path.
    // =========================================================================

    fn footnote_first_paragraph(xml: &str) -> ParagraphNode {
        use std::io::Cursor;
        let el = Element::parse(Cursor::new(xml.as_bytes())).expect("parse footnote element");
        let blocks = parse_note_blocks(&el, None, None, 720).expect("parse note blocks");
        blocks
            .into_iter()
            .find_map(|b| match b {
                BlockNode::Paragraph(p) => Some(*p),
                _ => None,
            })
            .expect("footnote should yield a paragraph")
    }

    #[test]
    fn story_paragraph_derives_heading_level_from_direct_outline_lvl() {
        // outlineLvl is 0-based on the wire (§17.3.1.20); heading levels are
        // 1-based, so outlineLvl=1 ⇒ heading level 2. This holds with no style
        // definitions loaded, isolating the direct-outlineLvl path.
        let xml = r#"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1">
            <w:p>
                <w:pPr><w:outlineLvl w:val="1"/></w:pPr>
                <w:r><w:t>A heading inside a footnote</w:t></w:r>
            </w:p>
        </w:footnote>"#;
        let para = footnote_first_paragraph(xml);
        assert_eq!(
            para.heading_level,
            Some(HeadingLevel::from_number(2)),
            "a footnote paragraph with outlineLvl=1 must resolve to heading level 2, \
             exactly as the body path does"
        );
    }

    #[test]
    fn story_paragraph_derives_heading_level_from_heading_style_id() {
        // Built-in Heading1–9 style ids map to the matching heading level even
        // without style definitions loaded (the mapping is by id name).
        let xml = r#"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1">
            <w:p>
                <w:pPr><w:pStyle w:val="Heading3"/></w:pPr>
                <w:r><w:t>Styled heading inside a footnote</w:t></w:r>
            </w:p>
        </w:footnote>"#;
        let para = footnote_first_paragraph(xml);
        assert_eq!(
            para.heading_level,
            Some(HeadingLevel::from_number(3)),
            "a footnote paragraph styled Heading3 must resolve to heading level 3, \
             exactly as the body path does"
        );
    }

    #[test]
    fn direct_outline_lvl_is_carried_on_paragraph_model() {
        // §17.3.1.20: a DIRECT w:pPr/w:outlineLvl must be carried verbatim
        // (0-based wire value) on the paragraph so the serializer can re-emit
        // it. Previously the value was consumed only to derive heading_level and
        // then discarded — a state-3 loss (the prod open→set_doc_defaults→save
        // path dropped w:outlineLvl). The model now carries it.
        let xml = r#"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1">
            <w:p>
                <w:pPr><w:outlineLvl w:val="3"/></w:pPr>
                <w:r><w:t>Direct outline level</w:t></w:r>
            </w:p>
        </w:footnote>"#;
        let para = footnote_first_paragraph(xml);
        assert_eq!(
            para.outline_lvl,
            Some(3),
            "a paragraph that directly authored w:outlineLvl must carry the \
             0-based wire value on the model"
        );
    }

    #[test]
    fn absent_outline_lvl_is_not_carried_on_paragraph_model() {
        // A paragraph that did NOT directly author w:outlineLvl must NOT carry
        // one (None). This mirrors the has_direct_* discipline: we never bake an
        // inherited cascade value as a direct property.
        let xml = r#"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1">
            <w:p><w:r><w:t>No outline level</w:t></w:r></w:p>
        </w:footnote>"#;
        let para = footnote_first_paragraph(xml);
        assert_eq!(
            para.outline_lvl, None,
            "a paragraph without a direct w:outlineLvl must not carry one"
        );
    }

    #[test]
    fn story_paragraph_without_heading_has_no_heading_level() {
        // A plain story paragraph (no outlineLvl, no Heading style) is not a
        // heading — guards against over-eagerly assigning a level.
        let xml = r#"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1">
            <w:p><w:r><w:t>Just footnote prose</w:t></w:r></w:p>
        </w:footnote>"#;
        let para = footnote_first_paragraph(xml);
        assert_eq!(
            para.heading_level, None,
            "a non-heading footnote paragraph must have no heading level"
        );
    }
}
