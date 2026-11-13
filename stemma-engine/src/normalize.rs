use std::collections::HashSet;

use quick_xml::Reader;
use quick_xml::events::Event;
use quick_xml::name::QName;
use xmltree::{Element, XMLNode};

use crate::docx::DocxArchive;
use crate::word_xml::is_w_tag;

/// WordprocessingML main namespace URI. Mirrors `word_xml::WORD_NS` (kept private
/// there); used by the streaming preflight scanner to resolve whether an element
/// is in the Word namespace without materializing an `xmltree` tree.
const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Element depth limit for the streaming preflight scan, mirroring
/// `word_xml::MAX_XML_ELEMENT_DEPTH` (entity-expansion / runaway-nesting defense).
/// Must stay in sync with that constant; see its doc comment for the rationale
/// behind the stack-safe value.
const MAX_XML_ELEMENT_DEPTH: usize = 512;

// =============================================================================
// Error types
// =============================================================================

#[derive(Debug)]
pub enum NormalizeError {
    XmlParse(xmltree::ParseError),
    /// quick-xml builder failure (the Approach-A parser path).
    XmlParseQuick(crate::word_xml::WordXmlError),
    XmlWrite(xmltree::Error),
    /// An auxiliary grid payload cannot be resolved without losing state.
    InvalidTableHistory {
        part: String,
        reason: String,
    },
    /// The package's main document part could not be located (OPC §9.3): a
    /// malformed `_rels/.rels`, a missing officeDocument relationship, an
    /// External target, or a dangling target. A real defect — normalizing the
    /// wrong part would silently corrupt output — so it is propagated, never
    /// defaulted away.
    Package(crate::docx_package::PackageError),
    /// A part carries a malformed tracked vertical cell merge (`w:cellMerge`,
    /// §17.13.5.3), which neither resolution can read.
    InvalidCellMerge {
        part: String,
        reason: String,
    },
}

/// Refuse a package any of whose revision-bearing parts carries a malformed
/// tracked vertical cell merge (`w:cellMerge`, §17.13.5.3): a `w:vMerge` or
/// `w:vMergeOrig` value outside ST_AnnotationVMerge (`rest`, `cont`), or a
/// cell with more than one merge-changing `w:cellMerge`. Both resolution
/// paths read a well-formed one ([`take_cell_merge`]); import refuses the
/// same shapes, so a document that opened never reaches resolution with one.
pub(crate) fn refuse_malformed_cell_merges(archive: &DocxArchive) -> Result<(), NormalizeError> {
    fn check(element: &Element) -> Result<(), String> {
        if is_w_tag(element, "tcPr") {
            let mut merge_changing = 0;
            for child in &element.children {
                let XMLNode::Element(child) = child else {
                    continue;
                };
                if is_w_tag(child, "cellMerge") && cell_merge_change(child)?.is_some() {
                    merge_changing += 1;
                }
            }
            if merge_changing > 1 {
                return Err(format!(
                    "a cell carries {merge_changing} merge-changing w:cellMerge elements; \
                     §17.13.5.3 allows one"
                ));
            }
        }
        for child in &element.children {
            if let XMLNode::Element(child) = child {
                check(child)?;
            }
        }
        Ok(())
    }
    let mut part_paths = collect_normalizable_part_paths(archive)?;
    part_paths.extend(collect_glossary_part_paths(archive)?);
    for part in part_paths {
        let Some(bytes) = archive.get(&part) else {
            continue;
        };
        // Cheap byte gate first; parse only a part that mentions the element.
        if memchr_find(bytes, b"cellMerge") {
            check(&parse_xml(bytes)?)
                .map_err(|reason| NormalizeError::InvalidCellMerge { part, reason })?;
        }
    }
    Ok(())
}

/// The merge states a `w:cellMerge` records, `(after, before)` from
/// `w:vMerge` / `w:vMergeOrig` (`None` attribute = not merged), or `None` for
/// one with neither attribute, which "can be ignored" (§17.13.5.3).
fn cell_merge_change(
    element: &Element,
) -> Result<Option<(crate::domain::VerticalMerge, crate::domain::VerticalMerge)>, String> {
    let merge = |name: &str| match crate::xml_attrs::attr_get(element, name).map(String::as_str) {
        None => Ok(crate::domain::VerticalMerge::None),
        Some("rest") => Ok(crate::domain::VerticalMerge::Restart),
        Some("cont") => Ok(crate::domain::VerticalMerge::Continue),
        Some(other) => Err(format!(
            "w:cellMerge {name}=\"{other}\" is not an ST_AnnotationVMerge value (rest, cont)"
        )),
    };
    let after = merge("w:vMerge")?;
    let before = merge("w:vMergeOrig")?;
    Ok((after != crate::domain::VerticalMerge::None
        || before != crate::domain::VerticalMerge::None)
        .then_some((after, before)))
}

/// Transient attribute marking a `w:tc` whose cell merge this resolution just
/// applied; never written out.
const RESOLVED_CELL_MERGE_MARK: &str = "stemma-resolved-cell-merge";

/// The rows of a table: its `w:tr` children, including rows inside row-level
/// wrappers (`w:sdt`, `w:customXml`), never a nested table's.
fn table_rows_mut(element: &mut Element) -> Vec<&mut Element> {
    let mut rows = Vec::new();
    for child in element.children.iter_mut() {
        let XMLNode::Element(el) = child else {
            continue;
        };
        if is_w_tag(el, "tr") {
            rows.push(el);
        } else if !is_w_tag(el, "tbl") && !is_w_tag(el, "tblPr") && !is_w_tag(el, "tblGrid") {
            rows.extend(table_rows_mut(el));
        }
    }
    rows
}

/// [`row_cells`], mutable: a row's cells including those inside a
/// cell-level content control (CT_SdtCell) or custom XML wrapper, in order.
fn row_cells_mut(tr: &mut Element) -> Vec<&mut Element> {
    let mut cells = Vec::new();
    for child in tr.children.iter_mut() {
        let XMLNode::Element(child) = child else {
            continue;
        };
        if is_w_tag(child, "tc") {
            cells.push(child);
        } else if is_w_tag(child, "sdt")
            || is_w_tag(child, "sdtContent")
            || is_w_tag(child, "customXml")
        {
            cells.extend(row_cells_mut(child));
        }
    }
    cells
}

fn w_val_u32(parent: &Element, local: &str) -> Option<u32> {
    parent.children.iter().find_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, local) => {
            crate::xml_attrs::attr_get(el, "w:val").and_then(|value| value.parse().ok())
        }
        _ => None,
    })
}

fn child_element<'a>(parent: &'a Element, local: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, local) => Some(el),
        _ => None,
    })
}

/// A row's cells with the grid column each starts at, its merge, and whether
/// a cell merge was just resolved on it.
fn row_merge_cells(row: &Element) -> Vec<(u32, crate::domain::VerticalMerge, bool)> {
    let mut column = child_element(row, "trPr")
        .and_then(|tr_pr| w_val_u32(tr_pr, "gridBefore"))
        .unwrap_or(0);
    let mut cells = Vec::new();
    for tc in row_cells(row) {
        let tc_pr = child_element(tc, "tcPr");
        let merge = match tc_pr.and_then(|tc_pr| child_element(tc_pr, "vMerge")) {
            None => crate::domain::VerticalMerge::None,
            Some(v_merge) => match crate::xml_attrs::attr_get(v_merge, "w:val").map(String::as_str)
            {
                Some("restart") => crate::domain::VerticalMerge::Restart,
                Some("continue") | None => crate::domain::VerticalMerge::Continue,
                Some(_) => crate::domain::VerticalMerge::None,
            },
        };
        let resolved = crate::xml_attrs::attr_get(tc, RESOLVED_CELL_MERGE_MARK).is_some();
        cells.push((column, merge, resolved));
        // A span past the last representable column ends there: no cell can
        // follow it in the grid, so nothing below can line up with one.
        column = column.saturating_add(
            tc_pr
                .and_then(|tc_pr| w_val_u32(tc_pr, "gridSpan"))
                .unwrap_or(1)
                .max(1),
        );
    }
    cells
}

/// Byte twin of `tracked_model::settle_resolved_cell_merges`: around each
/// cell of `tbl` whose merge change was just resolved, in its grid column
/// from the row above to the row below, a continuation with no merge above
/// it is not merged, then neither is a restart with no continuation below.
fn settle_resolved_cell_merges(tbl: &mut Element) {
    use crate::domain::VerticalMerge;
    let mut rows = table_rows_mut(tbl);
    let mut grid: Vec<Vec<(u32, VerticalMerge, bool)>> =
        rows.iter().map(|row| row_merge_cells(row)).collect();
    let mut affected = std::collections::BTreeSet::new();
    for (row, cells) in grid.iter().enumerate() {
        for (column, _, resolved) in cells {
            if *resolved {
                for neighbour in row.saturating_sub(1)..=(row + 1).min(grid.len() - 1) {
                    affected.insert((neighbour, *column));
                }
            }
        }
    }
    if affected.is_empty() {
        return;
    }
    let merge_at = |grid: &Vec<Vec<(u32, VerticalMerge, bool)>>, row: usize, column: u32| {
        grid[row]
            .iter()
            .find(|(start, _, _)| *start == column)
            .map(|(_, merge, _)| merge.clone())
    };
    let mut unmerge = Vec::new();
    for &(row, column) in &affected {
        if merge_at(&grid, row, column) == Some(VerticalMerge::Continue) {
            let above = row
                .checked_sub(1)
                .and_then(|above| merge_at(&grid, above, column));
            if !matches!(
                above,
                Some(VerticalMerge::Restart | VerticalMerge::Continue)
            ) {
                set_grid_merge(&mut grid, row, column, VerticalMerge::None);
                unmerge.push((row, column));
            }
        }
    }
    for &(row, column) in &affected {
        if merge_at(&grid, row, column) == Some(VerticalMerge::Restart) {
            let below = (row + 1 < grid.len())
                .then(|| merge_at(&grid, row + 1, column))
                .flatten();
            if below != Some(VerticalMerge::Continue) {
                set_grid_merge(&mut grid, row, column, VerticalMerge::None);
                unmerge.push((row, column));
            }
        }
    }
    for (row, column) in unmerge {
        let index = grid[row]
            .iter()
            .position(|(start, _, _)| *start == column)
            .expect("an unmerged cell was found in this grid");
        let tc = row_cells_mut(rows[row])
            .into_iter()
            .nth(index)
            .expect("the grid lists the row's cells in order");
        for child in tc.children.iter_mut() {
            if let XMLNode::Element(tc_pr) = child
                && is_w_tag(tc_pr, "tcPr")
            {
                tc_pr.children.retain(
                    |child| !matches!(child, XMLNode::Element(el) if is_w_tag(el, "vMerge")),
                );
            }
        }
    }
    for row in rows.iter_mut() {
        for tc in row_cells_mut(row) {
            tc.attributes
                .shift_remove(&crate::xml_attrs::attr_name(RESOLVED_CELL_MERGE_MARK));
        }
    }
}

fn set_grid_merge(
    grid: &mut [Vec<(u32, crate::domain::VerticalMerge, bool)>],
    row: usize,
    column: u32,
    merge: crate::domain::VerticalMerge,
) {
    if let Some(cell) = grid[row].iter_mut().find(|(start, _, _)| *start == column) {
        cell.1 = merge;
    }
}

/// Remove a `w:tc`'s `w:cellMerge` before its content is resolved,
/// returning the merge state a merge-changing one gives the cell (`accept`:
/// `w:vMerge`, reject: `w:vMergeOrig`); an attribute-less one "can be
/// ignored" (§17.13.5.3) and just goes. Byte twin of
/// `tracked_model::resolve_cell_merge`.
fn take_cell_merge(tc: &mut Element, accept: bool) -> Option<crate::domain::VerticalMerge> {
    let tc_pr = tc.children.iter_mut().find_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, "tcPr") => Some(el),
        _ => None,
    })?;
    let mut resolved = None;
    tc_pr.children.retain(|child| {
        let XMLNode::Element(el) = child else {
            return true;
        };
        if !is_w_tag(el, "cellMerge") {
            return true;
        }
        let change = cell_merge_change(el).expect(
            "cellMerge values are validated before resolution (refuse_malformed_cell_merges, import)",
        );
        if let Some((after, before)) = change {
            assert!(
                resolved.is_none(),
                "one merge-changing cellMerge per cell is validated before resolution"
            );
            resolved = Some(if accept { after } else { before });
        }
        false
    });
    resolved
}

/// Apply a resolved cell merge to a `w:tc` whose content is resolved: drop
/// any merge-changing `w:cellMerge` a restored prior tcPr brought back, set
/// the `w:vMerge` element, and empty a cell left as a continuation the way
/// Word does: its first paragraph stays with only its properties.
fn apply_cell_merge(tc: &mut Element, merge: crate::domain::VerticalMerge) {
    // The enclosing table settles the merges around this cell, then drops
    // the mark (`settle_resolved_cell_merges`).
    crate::xml_attrs::attr_set(tc, RESOLVED_CELL_MERGE_MARK, "");
    for child in tc.children.iter_mut() {
        let XMLNode::Element(tc_pr) = child else {
            continue;
        };
        if !is_w_tag(tc_pr, "tcPr") {
            continue;
        }
        tc_pr.children.retain(|child| {
            !matches!(child, XMLNode::Element(el)
                if (is_w_tag(el, "cellMerge") && matches!(cell_merge_change(el), Ok(Some(_))))
                    || is_w_tag(el, "vMerge"))
        });
        let v_merge = match merge {
            crate::domain::VerticalMerge::None => None,
            crate::domain::VerticalMerge::Restart => {
                let mut el = crate::word_xml::w_el("vMerge");
                crate::xml_attrs::attr_set(&mut el, "w:val", "restart");
                Some(el)
            }
            crate::domain::VerticalMerge::Continue => Some(crate::word_xml::w_el("vMerge")),
        };
        if let Some(v_merge) = v_merge {
            let order = crate::docx_validate_ordering::TCPR_ORDER;
            let rank = |name: &str| order.iter().position(|entry| *entry == name);
            let v_merge_rank = rank("vMerge").expect("vMerge is in TCPR_ORDER");
            let at = tc_pr
                .children
                .iter()
                .position(|child| {
                    matches!(child, XMLNode::Element(el)
                        if rank(local_element_name(el)).is_some_and(|r| r > v_merge_rank))
                })
                .unwrap_or(tc_pr.children.len());
            tc_pr.children.insert(at, XMLNode::Element(v_merge));
        }
    }
    if merge != crate::domain::VerticalMerge::Continue {
        return;
    }
    fn first_paragraph(element: &Element) -> Option<Element> {
        element.children.iter().find_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, "p") => Some(el.clone()),
            XMLNode::Element(el) if is_w_tag(el, "tcPr") || is_w_tag(el, "tbl") => None,
            XMLNode::Element(el) => first_paragraph(el),
            _ => None,
        })
    }
    let paragraph = first_paragraph(tc).map(|mut paragraph| {
        paragraph
            .children
            .retain(|child| matches!(child, XMLNode::Element(el) if is_w_tag(el, "pPr")));
        paragraph
    });
    tc.children
        .retain(|child| matches!(child, XMLNode::Element(el) if is_w_tag(el, "tcPr")));
    if let Some(paragraph) = paragraph {
        tc.children.push(XMLNode::Element(paragraph));
    }
}

// =============================================================================
// Relationship type constants (mirroring runtime.rs)
// =============================================================================

const HEADER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
const FOOTNOTES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
const ENDNOTES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
const COMMENTS_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
/// The glossary (building-blocks / AutoText) document part, referenced from the
/// document relationships (ECMA-376 Part 1 §17.12). It remains outside the
/// editable IR, but full accept/reject resolves it through the shared byte
/// kernel in both archive and model projection paths.
const GLOSSARY_DOCUMENT_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/glossaryDocument";

// =============================================================================
// Preflight Report types
// =============================================================================

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartRevisionCounts {
    pub ins: u32,
    pub del: u32,
    pub move_from: u32,
    pub move_to: u32,
    pub del_text: u32,
    pub format_pr_change: u32,
}

impl PartRevisionCounts {
    pub fn total(&self) -> u32 {
        self.ins + self.del + self.move_from + self.move_to + self.del_text + self.format_pr_change
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartCommentCounts {
    pub anchors: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartReport {
    pub part: String,
    pub revisions: PartRevisionCounts,
    pub comments: PartCommentCounts,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreflightTotals {
    pub revisions: PartRevisionCounts,
    pub comments: PartCommentCounts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightReport {
    pub parts: Vec<PartReport>,
    pub totals: PreflightTotals,
    /// Revision counts for glossary story parts (ECMA-376 §17.12), reported
    /// separately so callers can distinguish building-block revisions from the
    /// ordinary document stories. Accept/reject resolves these through the
    /// shared byte kernel; the glossary remains outside the editable IR.
    pub glossary: Vec<PartReport>,
    pub warnings: Vec<String>,
}

// =============================================================================
// Normalization Result types
// =============================================================================

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NormalizationResult {
    pub parts_normalized: Vec<String>,
    pub revisions_resolved: u32,
    pub opaque_nodes_resolved_revisions_count: u32,
    /// Compatibility field retained after glossary resolution support landed.
    /// Full accept/reject now resolves glossary revisions in both paths, so a
    /// successful operation returns zero.
    pub unresolved_glossary_revisions: u32,
}

// =============================================================================
// Rels parsing (local to this module)
// =============================================================================

fn relationship_target_to_part_path(target: &str) -> String {
    if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else {
        format!("word/{target}")
    }
}

/// The main document part name and its relationships-part path for a package
/// being normalized.
///
/// Normalization runs *downstream of import*, which has already enforced OPC
/// main-part resolution (ECMA-376 Part 2 §9.3). We re-resolve here so a package
/// whose main part is non-conventional (e.g. `word/document2.xml`) normalizes
/// the RIGHT part.
///
/// The ONLY tolerated shape is a package with no `_rels/.rels` at all — a bare
/// `word/document.xml` archive, which reaches this code only in isolated unit
/// fixtures, never in the post-import production path. For that shape we use the
/// conventional name rather than failing a mechanical transform. Every OTHER
/// resolution error (malformed `_rels/.rels`, missing officeDocument
/// relationship, External or dangling target) is a genuine defect: defaulting
/// would silently normalize the wrong part, so we propagate it.
fn main_part_and_rels(archive: &DocxArchive) -> Result<(String, String), NormalizeError> {
    match crate::docx_package::resolve_main_document_part(archive) {
        Ok(name) => {
            let rels = crate::docx_package::rels_part_path(&name);
            Ok((name, rels))
        }
        // No root relationships part at all: the bare-fixture shape only.
        Err(crate::docx_package::PackageError::MissingPart(ref p)) if p == "_rels/.rels" => Ok((
            "word/document.xml".to_string(),
            "word/_rels/document.xml.rels".to_string(),
        )),
        Err(e) => Err(NormalizeError::Package(e)),
    }
}

/// Collect every revision-capable story XML part path from the document
/// relationships file: the resolved main document part plus all headers,
/// footers, footnotes, endnotes, AND comments (`word/comments.xml`).
///
/// This is the single set both the preflight scan (which *counts* revisions)
/// and the accept-all / reject-all resolution paths (which *resolve* them)
/// enumerate — they MUST agree. If preflight tallied a revision in a part that
/// resolution then skipped, "accept all" would report success while leaving
/// pending revision markup behind (the exact divergence that motivated folding
/// comments into this set). Comment bodies (`w:comment` → `w:p` → runs) carry
/// `w:ins`/`w:del` like any other story, and the model resolution path
/// (`accept_all`/`reject_all` in `tracked_model.rs`) already projects them, so
/// the byte path must too to preserve wire/model equivalence.
///
/// The main part is located by the OPC officeDocument relationship (its name is
/// not fixed at word/document.xml); a package without a discoverable main part
/// is malformed and the error propagates rather than defaulting.
///
/// Glossary parts are collected separately by [`collect_glossary_part_paths`]
/// because they remain opaque package content rather than modeled stories.
fn collect_normalizable_part_paths(archive: &DocxArchive) -> Result<Vec<String>, NormalizeError> {
    let (main_part, rels_path) = main_part_and_rels(archive)?;
    let mut paths = vec![main_part];

    // A main part with no relationships part has no story siblings to collect.
    let Some(rels_xml) = archive.get(&rels_path) else {
        return Ok(paths);
    };

    let Ok(root) = parse_xml(rels_xml) else {
        return Ok(paths);
    };

    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        let local_name = local_element_name(el);
        if local_name != "Relationship" {
            continue;
        }

        let rel_type = match get_attr(el, "Type") {
            Some(t) => t,
            None => continue,
        };
        let target = match get_attr(el, "Target") {
            Some(t) => t,
            None => continue,
        };

        let should_include = rel_type == HEADER_REL_TYPE
            || rel_type == FOOTER_REL_TYPE
            || rel_type == FOOTNOTES_REL_TYPE
            || rel_type == ENDNOTES_REL_TYPE
            || rel_type == COMMENTS_REL_TYPE;

        if should_include {
            let part_path = relationship_target_to_part_path(target);
            if !paths.iter().any(|p| p == &part_path) {
                paths.push(part_path);
            }
        }
    }

    Ok(paths)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NoteKind {
    Footnote,
    Endnote,
    Comment,
}

impl NoteKind {
    fn relationship_type(self) -> &'static str {
        match self {
            Self::Footnote => FOOTNOTES_REL_TYPE,
            Self::Endnote => ENDNOTES_REL_TYPE,
            Self::Comment => COMMENTS_REL_TYPE,
        }
    }

    fn is_reference_tag(self, local_name: &str) -> bool {
        match self {
            Self::Footnote => local_name == "footnoteReference",
            Self::Endnote => local_name == "endnoteReference",
            // A range-only comment is a valid zero-width annotation shape.
            // Any surviving member of the anchor triple keeps its definition.
            Self::Comment => matches!(
                local_name,
                "commentReference" | "commentRangeStart" | "commentRangeEnd"
            ),
        }
    }

    fn definition_tag(self) -> &'static str {
        match self {
            Self::Footnote => "footnote",
            Self::Endnote => "endnote",
            Self::Comment => "comment",
        }
    }
}

fn collect_note_part_path(
    archive: &DocxArchive,
    kind: NoteKind,
) -> Result<Option<String>, NormalizeError> {
    let (_, rels_path) = main_part_and_rels(archive)?;
    let Some(rels_xml) = archive.get(&rels_path) else {
        return Ok(None);
    };
    let root = parse_xml(rels_xml)?;
    for child in &root.children {
        let XMLNode::Element(relationship) = child else {
            continue;
        };
        if local_element_name(relationship) != "Relationship"
            || get_attr(relationship, "Type") != Some(kind.relationship_type())
        {
            continue;
        }
        let Some(target) = get_attr(relationship, "Target") else {
            continue;
        };
        return Ok(Some(relationship_target_to_part_path(target)));
    }
    Ok(None)
}

fn collect_note_reference_ids(element: &Element, kind: NoteKind, ids: &mut HashSet<String>) {
    if kind.is_reference_tag(local_element_name(element))
        && let Some(id) = get_attr(element, "id")
    {
        ids.insert(id.to_string());
    }
    for child in &element.children {
        if let XMLNode::Element(child) = child {
            collect_note_reference_ids(child, kind, ids);
        }
    }
}

fn is_reserved_note_definition(element: &Element) -> bool {
    // These structural definitions need no body reference. In particular a
    // continuation notice must not be pruned as an orphaned user note.
    matches!(
        get_attr(element, "type"),
        Some("separator") | Some("continuationSeparator") | Some("continuationNotice")
    )
}

const COMMENT_EXTENSION_REL_TYPES: [&str; 3] = [
    "http://schemas.microsoft.com/office/2011/relationships/commentsExtended",
    "http://schemas.microsoft.com/office/2016/09/relationships/commentsIds",
    "http://schemas.microsoft.com/office/2018/08/relationships/commentsExtensible",
];

/// Whether a run child is content a comment's range can hold: anything but
/// the run's properties, a comment reference, the zero-width marks the model
/// reads as decorations (`import`'s decoration run children), and empty text.
/// Text, tabs, breaks, pictures, symbols and field characters all count —
/// Word 16 keeps a comment while anything remains in its range (observed),
/// the same rule as the model's `tracked_model::is_comment_range_content`.
fn is_comment_range_content(run_child: &Element) -> bool {
    match local_element_name(run_child) {
        "rPr"
        | "commentReference"
        | "annotationRef"
        | "footnoteRef"
        | "endnoteRef"
        | "separator"
        | "continuationSeparator"
        | "lastRenderedPageBreak"
        | "softHyphen" => false,
        "t" | "delText" | "instrText" | "delInstrText" => run_child
            .children
            .iter()
            .any(|child| matches!(child, XMLNode::Text(text) if !text.is_empty())),
        _ => true,
    }
}

/// The ids of the comments whose reference runs `root` holds, and of the
/// comments whose range holds content (`is_comment_range_content`) in it — at
/// any depth, so text inside a hyperlink or an inline content control counts.
fn comment_anchor_ids(root: &Element) -> (HashSet<String>, HashSet<String>) {
    fn walk(
        element: &Element,
        open: &mut HashSet<String>,
        references: &mut HashSet<String>,
        holding_content: &mut HashSet<String>,
    ) {
        let name = local_element_name(element);
        let id = get_attr(element, "id").map(str::to_string);
        match (name, id) {
            ("commentRangeStart", Some(id)) => {
                open.insert(id);
            }
            ("commentRangeEnd", Some(id)) => {
                open.remove(&id);
            }
            ("commentReference", Some(id)) => {
                references.insert(id);
            }
            ("r", _)
                if element.children.iter().any(
                    |child| matches!(child, XMLNode::Element(child) if is_comment_range_content(child)),
                ) =>
            {
                holding_content.extend(open.iter().cloned());
            }
            _ => {}
        }
        for child in &element.children {
            if let XMLNode::Element(child) = child {
                walk(child, open, references, holding_content);
            }
        }
    }
    let (mut references, mut holding_content) = (HashSet::new(), HashSet::new());
    walk(
        root,
        &mut HashSet::new(),
        &mut references,
        &mut holding_content,
    );
    (references, holding_content)
}

/// Accepting a move and the bookmarks of its source (Word 16, observed): a
/// bookmark goes, both halves, when it has a half in the moved text — or
/// spans moved text — and nothing it spans stays. A bookmark that also spans
/// content that stays is kept; Accept All then leaves its half in the moved
/// text where that text is removed. The same rule as the model's
/// `tracked_model::drop_bookmarks_of_accepted_move_sources`, applied before
/// Accept All removes the moved text:
/// - in the source: between `w:moveFromRangeStart` and `w:moveFromRangeEnd`,
///   inside a `w:moveFrom`, or in a whole moved-from paragraph (its mark is a
///   `w:moveFrom` and all its run content is moved or deleted);
/// - moved content: run content inside a `w:moveFrom` (or a `w:del` in the
///   source), and the mark of a whole moved-from paragraph;
/// - everything else spanned stays: other run content, other paragraphs'
///   marks, and a table with all it holds.
fn drop_bookmarks_of_move_sources(root: &mut Element) {
    #[derive(Default)]
    struct Span {
        touches_source: bool,
        holds_moved: bool,
        holds_staying: bool,
    }
    #[derive(Default)]
    struct State {
        inside_range: bool,
        open: std::collections::HashMap<String, Span>,
        doomed: HashSet<String>,
    }
    #[derive(Clone, Copy, Default)]
    struct Context {
        in_move_from: bool,
        in_del: bool,
        whole_source: bool,
        in_table: bool,
    }
    fn spanned(state: &mut State, moved: bool) {
        for span in state.open.values_mut() {
            if moved {
                span.holds_moved = true;
            } else {
                span.holds_staying = true;
            }
        }
    }
    fn run_has_content(run: &Element) -> bool {
        run.children.iter().any(
            |child| matches!(child, XMLNode::Element(child) if is_comment_range_content(child)),
        )
    }
    /// Whether `el` holds run content outside a `w:moveFrom` / `w:del`.
    fn holds_unremoved_content(el: &Element, removed: bool) -> bool {
        el.children.iter().any(|child| {
            let XMLNode::Element(child) = child else {
                return false;
            };
            match local_element_name(child) {
                "pPr" => false,
                "r" => !removed && run_has_content(child),
                "moveFrom" | "del" => holds_unremoved_content(child, true),
                _ => holds_unremoved_content(child, removed),
            }
        })
    }
    fn child<'a>(el: &'a Element, name: &str) -> Option<&'a Element> {
        el.children.iter().find_map(|c| match c {
            XMLNode::Element(c) if local_element_name(c) == name => Some(c),
            _ => None,
        })
    }
    fn mark_is_moved_from(paragraph: &Element) -> bool {
        child(paragraph, "pPr")
            .and_then(|ppr| child(ppr, "rPr"))
            .is_some_and(|rpr| child(rpr, "moveFrom").is_some())
    }
    fn walk(el: &Element, ctx: Context, state: &mut State) {
        for child in &el.children {
            let XMLNode::Element(child) = child else {
                continue;
            };
            let in_source =
                !ctx.in_table && (state.inside_range || ctx.in_move_from || ctx.whole_source);
            match local_element_name(child) {
                "moveFromRangeStart" => state.inside_range = true,
                "moveFromRangeEnd" => state.inside_range = false,
                name @ ("bookmarkStart" | "bookmarkEnd") => {
                    let Some(id) = get_attr(child, "id").map(str::to_string) else {
                        continue;
                    };
                    if name == "bookmarkStart" {
                        state.open.insert(
                            id,
                            Span {
                                touches_source: in_source,
                                ..Span::default()
                            },
                        );
                    } else if let Some(span) = state.open.remove(&id)
                        && (span.touches_source || in_source || span.holds_moved)
                        && !span.holds_staying
                    {
                        state.doomed.insert(id);
                    }
                }
                "pPr" | "sectPr" => {}
                "r" => {
                    if run_has_content(child) {
                        let moved =
                            !ctx.in_table && (ctx.in_move_from || (ctx.in_del && in_source));
                        spanned(state, moved);
                    }
                }
                "tbl" => {
                    spanned(state, false);
                    walk(
                        child,
                        Context {
                            in_table: true,
                            ..ctx
                        },
                        state,
                    );
                }
                "p" => {
                    let whole_source = !ctx.in_table
                        && mark_is_moved_from(child)
                        && !holds_unremoved_content(child, false);
                    walk(
                        child,
                        Context {
                            whole_source,
                            ..ctx
                        },
                        state,
                    );
                    // The paragraph's mark.
                    spanned(state, whole_source);
                }
                "moveFrom" => walk(
                    child,
                    Context {
                        in_move_from: true,
                        ..ctx
                    },
                    state,
                ),
                "del" => walk(
                    child,
                    Context {
                        in_del: true,
                        ..ctx
                    },
                    state,
                ),
                _ => walk(child, ctx, state),
            }
        }
    }
    fn strip(el: &mut Element, doomed: &HashSet<String>) {
        el.children.retain(|child| {
            !matches!(child, XMLNode::Element(child)
                if matches!(local_element_name(child), "bookmarkStart" | "bookmarkEnd")
                    && get_attr(child, "id").is_some_and(|id| doomed.contains(id)))
        });
        for child in &mut el.children {
            if let XMLNode::Element(child) = child {
                strip(child, doomed);
            }
        }
    }
    let mut state = State::default();
    walk(root, Context::default(), &mut state);
    if !state.doomed.is_empty() {
        strip(root, &state.doomed);
    }
}

/// Unwrap each `w:ins` whose only content is comment reference runs for
/// comments whose range holds no inserted text: Word lists no revision for it
/// and keeps the reference (and its comment) on Accept and on Reject alike, as
/// the import does (`import::untrack_reference_only_insertions`).
fn untrack_reference_only_insertions(root: &mut Element) {
    fn ranges_with_insertions(
        element: &Element,
        inserted: bool,
        open: &mut HashSet<String>,
        out: &mut HashSet<String>,
    ) {
        let name = local_element_name(element);
        match (name, get_attr(element, "id")) {
            ("commentRangeStart", Some(id)) => {
                open.insert(id.to_string());
            }
            ("commentRangeEnd", Some(id)) => {
                open.remove(id);
            }
            ("t" | "delText", _) if inserted => out.extend(open.iter().cloned()),
            _ => {}
        }
        let inserted = inserted || name == "ins";
        for child in &element.children {
            if let XMLNode::Element(child) = child {
                ranges_with_insertions(child, inserted, open, out);
            }
        }
    }
    fn reference_only(ins: &Element, ranges: &HashSet<String>) -> bool {
        let mut any = false;
        let all = ins.children.iter().all(|child| match child {
            XMLNode::Element(run) if local_element_name(run) == "r" => {
                run.children.iter().all(|c| match c {
                    XMLNode::Element(e) if local_element_name(e) == "rPr" => true,
                    XMLNode::Element(e) if local_element_name(e) == "commentReference" => {
                        any = true;
                        get_attr(e, "id").is_some_and(|id| !ranges.contains(id))
                    }
                    XMLNode::Element(_) => false,
                    _ => true,
                })
            }
            XMLNode::Element(_) => false,
            _ => true,
        });
        all && any
    }
    fn unwrap(element: &mut Element, ranges: &HashSet<String>) {
        let mut children = Vec::with_capacity(element.children.len());
        for child in element.children.drain(..) {
            match child {
                XMLNode::Element(ins)
                    if local_element_name(&ins) == "ins" && reference_only(&ins, ranges) =>
                {
                    children.extend(ins.children);
                }
                XMLNode::Element(mut other) => {
                    unwrap(&mut other, ranges);
                    children.push(XMLNode::Element(other));
                }
                other => children.push(other),
            }
        }
        element.children = children;
    }
    let mut ranges = HashSet::new();
    ranges_with_insertions(root, false, &mut HashSet::new(), &mut ranges);
    unwrap(root, &ranges);
}

/// Remove the comments a resolution took the anchor of, as Word does (and as
/// the model's projection does, `tracked_model::remove_comments_that_lost_their_anchor`):
/// a comment whose reference the resolution removed, or whose range lost all
/// the content it held, goes whole. Its range markers and reference runs leave
/// every story part, its definition leaves the comments part, and its
/// `commentsExtended` / `commentsIds` / `commentsExtensible` entries go too.
fn remove_comments_that_lost_their_anchor(
    archive: &mut DocxArchive,
    original: &DocxArchive,
    story_part_paths: &[String],
) -> Result<Vec<String>, NormalizeError> {
    let mut changed = Vec::new();
    let Some(comments_path) = collect_note_part_path(archive, NoteKind::Comment)? else {
        return Ok(changed);
    };
    let mut lost = HashSet::new();
    for story_path in story_part_paths {
        if *story_path == comments_path {
            continue;
        }
        let (Some(before), Some(after)) = (original.get(story_path), archive.get(story_path))
        else {
            continue;
        };
        let (references_before, content_before) = comment_anchor_ids(&parse_xml(before)?);
        let (references_after, content_after) = comment_anchor_ids(&parse_xml(after)?);
        lost.extend(references_before.difference(&references_after).cloned());
        lost.extend(content_before.difference(&content_after).cloned());
    }
    if lost.is_empty() {
        return Ok(changed);
    }

    fn strip(element: &mut Element, lost: &HashSet<String>) -> bool {
        let before = element.children.len();
        element.children.retain(|child| {
            let XMLNode::Element(child) = child else {
                return true;
            };
            let name = local_element_name(child);
            let anchor = |element: &Element| {
                matches!(
                    local_element_name(element),
                    "commentRangeStart" | "commentRangeEnd" | "commentReference"
                ) && get_attr(element, "id").is_some_and(|id| lost.contains(id))
            };
            if anchor(child) {
                return false;
            }
            // A run holding only a removed reference (and its properties)
            // goes with it.
            !(name == "r"
                && child
                    .children
                    .iter()
                    .any(|c| matches!(c, XMLNode::Element(e) if anchor(e)))
                && child.children.iter().all(|c| match c {
                    XMLNode::Element(e) => local_element_name(e) == "rPr" || anchor(e),
                    _ => true,
                }))
        });
        let mut changed = element.children.len() != before;
        for child in &mut element.children {
            if let XMLNode::Element(child) = child {
                changed |= strip(child, lost);
            }
        }
        changed
    }
    for story_path in story_part_paths {
        if *story_path == comments_path {
            continue;
        }
        let Some(bytes) = archive.get(story_path) else {
            continue;
        };
        let mut root = parse_xml(bytes)?;
        if strip(&mut root, &lost) {
            archive.upsert(story_path, write_xml(&root)?);
            changed.push(story_path.clone());
        }
    }

    // The removed comments' paragraph ids key their extension entries.
    let Some(comments_bytes) = archive.get(&comments_path) else {
        return Ok(changed);
    };
    let mut comments = parse_xml(comments_bytes)?;
    let mut para_ids = HashSet::new();
    comments.children.retain(|child| {
        let XMLNode::Element(comment) = child else {
            return true;
        };
        let removed = local_element_name(comment) == "comment"
            && get_attr(comment, "id").is_some_and(|id| lost.contains(id));
        if removed {
            for paragraph in &comment.children {
                if let XMLNode::Element(paragraph) = paragraph
                    && let Some(para_id) = get_attr(paragraph, "paraId")
                {
                    para_ids.insert(para_id.to_string());
                }
            }
        }
        !removed
    });
    archive.upsert(&comments_path, write_xml(&comments)?);
    changed.push(comments_path);

    let (_, rels_path) = main_part_and_rels(archive)?;
    let mut extension_paths = Vec::new();
    if let Some(rels_xml) = archive.get(&rels_path) {
        let rels = parse_xml(rels_xml)?;
        for child in &rels.children {
            if let XMLNode::Element(relationship) = child
                && get_attr(relationship, "Type")
                    .is_some_and(|kind| COMMENT_EXTENSION_REL_TYPES.contains(&kind))
                && let Some(target) = get_attr(relationship, "Target")
            {
                extension_paths.push(relationship_target_to_part_path(target));
            }
        }
    }
    // commentsIds maps each paragraph id to the durable id commentsExtensible
    // keys on: collect those first.
    let mut durable_ids = HashSet::new();
    for path in &extension_paths {
        let Some(bytes) = archive.get(path) else {
            continue;
        };
        let root = parse_xml(bytes)?;
        for child in &root.children {
            if let XMLNode::Element(entry) = child
                && get_attr(entry, "paraId").is_some_and(|id| para_ids.contains(id))
                && let Some(durable) = get_attr(entry, "durableId")
            {
                durable_ids.insert(durable.to_string());
            }
        }
    }
    for path in extension_paths {
        let Some(bytes) = archive.get(&path) else {
            continue;
        };
        let mut root = parse_xml(bytes)?;
        let before = root.children.len();
        root.children.retain(|child| {
            let XMLNode::Element(entry) = child else {
                return true;
            };
            !(get_attr(entry, "paraId").is_some_and(|id| para_ids.contains(id))
                || get_attr(entry, "durableId").is_some_and(|id| durable_ids.contains(id)))
        });
        if root.children.len() != before {
            archive.upsert(&path, write_xml(&root)?);
            changed.push(path);
        }
    }
    Ok(changed)
}

/// Reconcile note/comment definitions with the references that survive
/// accept/reject.
///
/// A tracked InsertNote wraps the body reference in `w:ins`, while its backing
/// `w:footnote`/`w:endnote` definition is ordinary story content. Rejecting the
/// insertion therefore removes the reference but cannot remove the definition
/// through revision-element descent alone. The inverse shape occurs when a
/// tracked note deletion is accepted. Leaving either orphan makes the archive
/// resolver disagree with the typed projection and can trigger a Word repair.
fn prune_unreferenced_note_definitions(
    archive: &mut DocxArchive,
    original: &DocxArchive,
    story_part_paths: &[String],
) -> Result<Vec<String>, NormalizeError> {
    let mut changed_parts = Vec::new();
    for kind in [NoteKind::Footnote, NoteKind::Endnote, NoteKind::Comment] {
        let Some(note_part_path) = collect_note_part_path(archive, kind)? else {
            continue;
        };
        let mut referenced_ids = HashSet::new();
        let mut originally_referenced_ids = HashSet::new();
        for story_path in story_part_paths {
            let Some(bytes) = archive.get(story_path) else {
                continue;
            };
            let root = parse_xml(bytes)?;
            collect_note_reference_ids(&root, kind, &mut referenced_ids);
            if let Some(original_bytes) = original.get(story_path) {
                let original_root = parse_xml(original_bytes)?;
                collect_note_reference_ids(&original_root, kind, &mut originally_referenced_ids);
            }
        }

        let Some(note_bytes) = archive.get(&note_part_path) else {
            continue;
        };
        let mut root = parse_xml(note_bytes)?;
        let before = root.children.len();
        root.children.retain(|child| {
            let XMLNode::Element(note) = child else {
                return true;
            };
            if local_element_name(note) != kind.definition_tag()
                || is_reserved_note_definition(note)
            {
                return true;
            }
            let Some(id) = get_attr(note, "id") else {
                return true;
            };
            // Only a definition this resolution took the last reference of
            // goes; one that had none before is the input's own state (as
            // the model projection rules, `prune_newly_unreferenced_notes`).
            referenced_ids.contains(id) || !originally_referenced_ids.contains(id)
        });
        if root.children.len() != before {
            archive.upsert(&note_part_path, write_xml(&root)?);
            changed_parts.push(note_part_path);
        }
    }
    Ok(changed_parts)
}

/// Collect the glossary story part path(s) from the document relationships — the
/// parts that carry building blocks / AutoText (ECMA-376 §17.12) and can legally
/// hold tracked revisions inside their doc-part bodies (`CT_Body`).
///
/// This is the SINGLE glossary collector both the preflight scan and the
/// resolution paths use. Keeping it one function is the same discipline that
/// governs the normalizable set: preflight and mutation cannot drift.
///
/// Scope: the glossary main document part, discovered by the `glossaryDocument`
/// relationship (robust to a non-conventional target name), plus its own
/// header/footer/footnote/endnote/comment sub-stories discovered through the
/// glossary relationship part.
fn collect_glossary_part_paths(archive: &DocxArchive) -> Result<Vec<String>, NormalizeError> {
    let (main_part, rels_path) = main_part_and_rels(archive)?;
    let main_dir = main_part
        .rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/"))
        .unwrap_or_default();

    let mut paths = Vec::new();
    let Some(rels_xml) = archive.get(&rels_path) else {
        return Ok(paths);
    };
    let Ok(root) = parse_xml(rels_xml) else {
        return Ok(paths);
    };

    for child in &root.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        if local_element_name(el) != "Relationship" {
            continue;
        }
        let Some(rel_type) = get_attr(el, "Type") else {
            continue;
        };
        if rel_type != GLOSSARY_DOCUMENT_REL_TYPE {
            continue;
        }
        let Some(target) = get_attr(el, "Target") else {
            continue;
        };
        let part_path = crate::import::resolve_relationship_target(target, &main_dir);
        if !paths.iter().any(|p| p == &part_path) {
            paths.push(part_path);
        }
    }

    let glossary_mains = paths.clone();
    for glossary_main in glossary_mains {
        let rels_path = crate::docx_package::rels_part_path(&glossary_main);
        let Some(rels_xml) = archive.get(&rels_path) else {
            continue;
        };
        let root = parse_xml(rels_xml)?;
        let base_dir = glossary_main
            .rsplit_once('/')
            .map(|(dir, _)| format!("{dir}/"))
            .unwrap_or_default();
        for child in &root.children {
            let XMLNode::Element(el) = child else {
                continue;
            };
            if local_element_name(el) != "Relationship" {
                continue;
            }
            let Some(rel_type) = get_attr(el, "Type") else {
                continue;
            };
            if !matches!(
                rel_type,
                HEADER_REL_TYPE
                    | FOOTER_REL_TYPE
                    | FOOTNOTES_REL_TYPE
                    | ENDNOTES_REL_TYPE
                    | COMMENTS_REL_TYPE
            ) {
                continue;
            }
            let Some(target) = get_attr(el, "Target") else {
                continue;
            };
            let part_path = crate::import::resolve_relationship_target(target, &base_dir);
            if !paths.iter().any(|path| path == &part_path) {
                paths.push(part_path);
            }
        }
    }

    Ok(paths)
}

/// Scan the glossary story part(s) and return a [`PartReport`] per part that
/// carries revisions. Reused by [`preflight_scan`] to disclose them in
/// [`PreflightReport::glossary`]. Resolution enumerates through the same
/// collector, so discovery and mutation cannot drift.
fn scan_glossary_parts(archive: &DocxArchive) -> Result<Vec<PartReport>, NormalizeError> {
    let mut reports = Vec::new();
    for part_path in collect_glossary_part_paths(archive)? {
        let Some(xml_bytes) = archive.get(&part_path) else {
            continue;
        };
        let (revisions, comments) = scan_part_streaming(xml_bytes)?;
        if revisions.total() > 0 {
            reports.push(PartReport {
                part: part_path,
                revisions,
                comments,
            });
        }
    }
    Ok(reports)
}

// =============================================================================
// XML helpers (local to this module)
// =============================================================================

fn parse_xml(bytes: &[u8]) -> Result<Element, NormalizeError> {
    // Approach A: use the quick-xml builder (same `Element` shape, no per-element
    // namespace clone). Normalization parses document.xml-sized parts on the
    // import hot path whenever the document carries tracked changes, so this is
    // the second-largest parse after `parse_document_xml`.
    crate::word_xml::parse_document_xml_quick(bytes).map_err(NormalizeError::XmlParseQuick)
}

fn write_xml(element: &Element) -> Result<Vec<u8>, NormalizeError> {
    let mut out = Vec::new();
    element.write(&mut out).map_err(NormalizeError::XmlWrite)?;
    Ok(out)
}

/// Resolve the auxiliary grid before property owners are removed by the
/// content walk. Nested tables own their own frames. This deliberately does
/// not count tblGridChange as an independently selectable revision.
fn resolve_table_grid_history(
    root: &mut Element,
    accept: bool,
    part: &str,
) -> Result<u32, NormalizeError> {
    fn owns_properties(element: &Element) -> bool {
        element
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .any(|child| {
                if is_w_tag(child, "tbl") || is_w_tag(child, "p") {
                    return false;
                }
                ["tblPrChange", "tblPrExChange", "trPrChange", "tcPrChange"]
                    .iter()
                    .any(|tag| is_w_tag(child, tag))
                    || owns_properties(child)
            })
    }
    let mut resolved = 0;
    if is_w_tag(root, "tbl") {
        resolve_simple_column_slots(root, accept, part)?;
        let restore_previous = !accept && owns_properties(root);
        for grid in root
            .children
            .iter_mut()
            .filter_map(XMLNode::as_mut_element)
            .filter(|child| is_w_tag(child, "tblGrid"))
        {
            let history = crate::import::parse_grid_history(grid).map_err(|error| {
                NormalizeError::InvalidTableHistory {
                    part: part.to_owned(),
                    reason: error.message,
                }
            })?;
            if history.is_none() {
                continue;
            }
            // The shared parser has proved exactly one record with exactly
            // one previous tblGrid. Preserve its XML spellings, not a second
            // reconstruction of the already-validated widths.
            let index = grid
                .children
                .iter()
                .position(|node| {
                    node.as_element()
                        .is_some_and(|el| is_w_tag(el, "tblGridChange"))
                })
                .expect("validated auxiliary grid record");
            let XMLNode::Element(change) = grid.children.remove(index) else {
                unreachable!("validated auxiliary grid element")
            };
            if restore_previous {
                let previous = change
                    .children
                    .into_iter()
                    .find_map(|node| match node {
                        XMLNode::Element(el) => Some(el),
                        _ => None,
                    })
                    .expect("validated previous table grid");
                grid.children = previous.children;
            }
            resolved += 1;
        }
    }
    if is_w_tag(root, "tblGridChange") {
        return Err(NormalizeError::InvalidTableHistory {
            part: part.to_owned(),
            reason: "tblGridChange is outside the direct grid of a table".to_owned(),
        });
    }
    for child in root.children.iter_mut().filter_map(XMLNode::as_mut_element) {
        resolved += resolve_table_grid_history(child, accept, part)?;
    }
    Ok(resolved)
}

/// Reconcile the current simple-grid frame before removing cell carriers or
/// restoring previous property history. Non-simple grids have row-local span
/// semantics and do not represent whole-column removal in this coordinate frame.
fn resolve_simple_column_slots(
    table: &mut Element,
    accept: bool,
    part: &str,
) -> Result<(), NormalizeError> {
    fn child<'a>(element: &'a Element, tag: &str) -> Option<&'a Element> {
        element
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .find(|element| is_w_tag(element, tag))
    }
    fn value(element: &Element, part: &str) -> Result<u32, NormalizeError> {
        // ISO 29500-1 §17.4.16 explicitly defines an omitted saved grid width
        // as zero. Absence is valid; a present malformed value is not. A
        // present value reads as import reads it.
        let Some(width) = get_attr(element, "w") else {
            return Ok(0);
        };
        crate::import::parse_grid_col_width(width, &element.name).map_err(|error| {
            NormalizeError::InvalidTableHistory {
                part: part.to_owned(),
                reason: error.message,
            }
        })
    }
    let Some(grid) = child(table, "tblGrid") else {
        return Ok(());
    };
    let widths: Vec<_> = grid
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|element| is_w_tag(element, "gridCol"))
        .map(|element| value(element, part))
        .collect::<Result<_, _>>()?;
    if widths.is_empty() {
        return Ok(());
    }
    let rows: Vec<_> = table
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|element| is_w_tag(element, "tr"))
        .filter(|row| {
            let properties = child(row, "trPr");
            let inserted = properties.is_some_and(|properties| child(properties, "ins").is_some());
            let deleted = properties.is_some_and(|properties| child(properties, "del").is_some());
            crate::resolution_rules::tracked_class_survives(inserted, deleted, accept)
        })
        .collect();
    let cells: Vec<Vec<_>> = rows
        .iter()
        .map(|row| {
            row.children
                .iter()
                .filter_map(XMLNode::as_element)
                .filter(|element| is_w_tag(element, "tc"))
                .collect()
        })
        .collect();
    if cells.is_empty()
        || rows.iter().zip(&cells).any(|(row, cells)| {
            cells.len() != widths.len()
                || child(row, "trPr").is_some_and(|properties| {
                    ["gridBefore", "gridAfter"].iter().any(|tag| {
                        child(properties, tag)
                            .is_some_and(|element| get_attr(element, "val") != Some("0"))
                    })
                })
                || cells.iter().any(|cell| {
                    child(cell, "tcPr").is_some_and(|properties| {
                        child(properties, "vMerge").is_some()
                            || child(properties, "gridSpan")
                                .is_some_and(|element| get_attr(element, "val") != Some("1"))
                    })
                })
        })
    {
        return Ok(());
    }
    let removed: Vec<_> = (0..widths.len())
        .map(|index| {
            cells.iter().all(|row| {
                !crate::resolution_rules::tracked_class_survives(
                    has_cell_tracking(row[index], "cellIns"),
                    has_cell_tracking(row[index], "cellDel"),
                    accept,
                )
            })
        })
        .collect();
    if !removed.iter().any(|removed| *removed) {
        return Ok(());
    }
    // A paragraph whose mark is not tracked as deleted.
    let keeps_mark = |paragraph: &Element| {
        !child(paragraph, "pPr")
            .and_then(|properties| child(properties, "rPr"))
            .is_some_and(|properties| child(properties, "del").is_some())
    };
    let absorb: Vec<_> = (0..widths.len())
        .map(|index| {
            accept
                && cells.iter().all(|row| {
                    has_cell_tracking(row[index], "cellDel")
                        && !has_cell_tracking(row[index], "cellIns")
                        && crate::resolution_rules::deleted_cell_gives_width(
                            row[index]
                                .children
                                .iter()
                                .filter_map(XMLNode::as_element)
                                .filter(|element| is_w_tag(element, "p"))
                                .any(keeps_mark),
                        )
                })
        })
        .collect();
    let slots = crate::resolution_rules::resolved_column_slots(&widths, &removed, &absorb)
        .map_err(|reason| NormalizeError::InvalidTableHistory {
            part: part.to_owned(),
            reason,
        })?;
    for row in table
        .children
        .iter_mut()
        .filter_map(XMLNode::as_mut_element)
        .filter(|element| is_w_tag(element, "tr"))
    {
        for (index, cell) in row
            .children
            .iter_mut()
            .filter_map(XMLNode::as_mut_element)
            .filter(|element| is_w_tag(element, "tc"))
            .enumerate()
        {
            if !accept {
                continue;
            }
            let Some(&(_, width)) = slots.iter().find(|(slot, _)| *slot == index) else {
                continue;
            };
            if width == widths[index] {
                continue;
            }
            if child(cell, "tcPr").is_none() {
                cell.children
                    .insert(0, XMLNode::Element(crate::word_xml::w_el("tcPr")));
            }
            let properties = cell
                .children
                .iter_mut()
                .filter_map(XMLNode::as_mut_element)
                .find(|element| is_w_tag(element, "tcPr"))
                .expect("cell properties inserted");
            if child(properties, "tcW").is_none() {
                properties
                    .children
                    .insert(0, XMLNode::Element(crate::word_xml::w_el("tcW")));
            }
            let measurement = properties
                .children
                .iter_mut()
                .filter_map(XMLNode::as_mut_element)
                .find(|element| is_w_tag(element, "tcW"))
                .expect("cell width inserted");
            crate::xml_attrs::attr_set(measurement, "w:w", width.to_string());
            crate::xml_attrs::attr_set(measurement, "w:type", "dxa");
        }
    }
    let grid = table
        .children
        .iter_mut()
        .filter_map(XMLNode::as_mut_element)
        .find(|element| is_w_tag(element, "tblGrid"))
        .expect("current grid exists");
    let mut index = 0;
    grid.children.retain_mut(|node| {
        let XMLNode::Element(element) = node else {
            return true;
        };
        if !is_w_tag(element, "gridCol") {
            return true;
        }
        let slot = index;
        index += 1;
        let Some(&(_, width)) = slots.iter().find(|(index, _)| *index == slot) else {
            return false;
        };
        crate::xml_attrs::attr_set(element, "w:w", width.to_string());
        true
    });
    Ok(())
}

fn local_element_name(element: &Element) -> &str {
    if let Some(pos) = element.name.find(':') {
        &element.name[pos + 1..]
    } else {
        &element.name
    }
}

fn get_attr<'a>(element: &'a Element, local: &str) -> Option<&'a str> {
    // Try namespaced attribute lookup first
    for (name, value) in &element.attributes {
        if name.local_name == local {
            return Some(value.as_str());
        }
    }
    // Fallback: try with the full qname as local_name (e.g., "w:id" stored as local)
    None
}

// =============================================================================
// Revision element classification
// =============================================================================

/// Content revision tags that are resolved during normalization.
const CONTENT_REVISION_TAGS: &[&str] = &["ins", "del", "moveFrom", "moveTo"];

/// Property change tags that are resolved during normalization.
const PR_CHANGE_TAGS: &[&str] = &[
    "rPrChange",
    "pPrChange",
    "tblPrChange",
    "tblPrExChange",
    "trPrChange",
    "tcPrChange",
    "sectPrChange",
];

/// Check if an element is a content revision wrapper (w:ins, w:del, w:moveFrom, w:moveTo).
fn is_content_revision(element: &Element) -> bool {
    CONTENT_REVISION_TAGS
        .iter()
        .any(|tag| is_w_tag(element, tag))
}

/// Check if an element is a property change (w:rPrChange, w:pPrChange, etc.)
fn is_pr_change(element: &Element) -> bool {
    PR_CHANGE_TAGS.iter().any(|tag| is_w_tag(element, tag))
}

/// Check if an element is w:delText.
fn is_del_text(element: &Element) -> bool {
    is_w_tag(element, "delText")
}

/// The run-content element local-name pairs whose spelling differs between
/// deleted tracked content and plain content: `(plain, deleted)`. `w:t` ↔
/// `w:delText` (§17.4.20) and `w:instrText` ↔ `w:delInstrText` (§17.16.13) are
/// the ONLY run-content elements whose name flips when content crosses the
/// deleted (`w:del`) boundary.
///
/// This is the single source of truth for every coercion in BOTH directions —
/// the forward rewrite that wraps content in `w:del`
/// (`serialize::coerce_opaque_run_text` with `deleted = true`) and the inverse
/// that restores it on reject (this module's `convert_del_text_to_t` and
/// `serialize::coerce_opaque_run_text` with `deleted = false`) — so the two
/// directions can never drift into disagreeing whitelists. Emitting a
/// `w:delText`/`w:delInstrText` outside `w:del` ancestry is schema-invalid and
/// makes Word repair the file on open (guarded by
/// `runtime::enforce_story_deleted_text_integrity`).
pub(crate) const DELETED_RUN_CONTENT_PAIRS: [(&str, &str); 2] =
    [("t", "delText"), ("instrText", "delInstrText")];

/// The deleted-form local name for a plain run-content local name, if any
/// (`t` → `delText`, `instrText` → `delInstrText`).
pub(crate) fn deleted_run_content_name(plain_local: &str) -> Option<&'static str> {
    DELETED_RUN_CONTENT_PAIRS
        .iter()
        .find(|(plain, _)| *plain == plain_local)
        .map(|(_, deleted)| *deleted)
}

/// The plain-form local name for a deleted run-content local name, if any
/// (`delText` → `t`, `delInstrText` → `instrText`).
pub(crate) fn plain_run_content_name(deleted_local: &str) -> Option<&'static str> {
    DELETED_RUN_CONTENT_PAIRS
        .iter()
        .find(|(_, deleted)| *deleted == deleted_local)
        .map(|(plain, _)| *plain)
}

/// Move range delimiters (§17.13.5.24–28): `w:moveFromRangeStart/End`,
/// `w:moveToRangeStart/End`. They bracket the moved region and are part of
/// the move revision — resolving the move (either direction) must remove
/// them along with the `w:moveFrom`/`w:moveTo` content wrappers. Left
/// behind, the importer reads paragraphs between `moveToRangeStart/End` as
/// still-pending block insertions.
fn is_move_range_marker(element: &Element) -> bool {
    [
        "moveFromRangeStart",
        "moveFromRangeEnd",
        "moveToRangeStart",
        "moveToRangeEnd",
    ]
    .iter()
    .any(|tag| is_w_tag(element, tag))
}

fn custom_xml_deletion_range_id<'a>(element: &'a Element, local_name: &str) -> Option<&'a str> {
    is_w_tag(element, local_name)
        .then(|| get_attr(element, "id"))
        .flatten()
}

/// Whether `children[index..index + 3]` is the bounded inline-SDT deletion
/// carrier proved by W-SDT-INLINE-DELETE-01:
///
/// `customXmlDelRangeStart`, one `w:sdt` whose complete `w:sdtContent` is one
/// `w:del`, then the matching `customXmlDelRangeEnd`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeletedInlineSdtEnvelope {
    /// Engine-emitted pre-Word-save form: the range encloses the whole SDT.
    Direct,
    /// Word-save form: the incoming range ends at the start of sdtContent and
    /// a new outgoing range begins after the deletion inside sdtContent.
    WordNormalized,
}

fn deleted_inline_sdt_envelope_at(
    children: &[XMLNode],
    index: usize,
) -> Option<DeletedInlineSdtEnvelope> {
    let [
        XMLNode::Element(start),
        XMLNode::Element(sdt),
        XMLNode::Element(end),
    ] = children.get(index..index.saturating_add(3)).unwrap_or(&[])
    else {
        return None;
    };
    let start_id = custom_xml_deletion_range_id(start, "customXmlDelRangeStart")?;
    if !is_w_tag(sdt, "sdt") {
        return None;
    }
    let contents = sdt
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|child| is_w_tag(child, "sdtContent"))
        .collect::<Vec<_>>();
    if contents.len() != 1 {
        return None;
    }
    let meaningful = contents[0]
        .children
        .iter()
        .filter(|child| match child {
            XMLNode::Text(text) | XMLNode::CData(text) => !text.trim().is_empty(),
            XMLNode::Comment(_) => false,
            _ => true,
        })
        .collect::<Vec<_>>();
    match meaningful.as_slice() {
        [XMLNode::Element(deletion)]
            if is_w_tag(deletion, "del")
                && custom_xml_deletion_range_id(end, "customXmlDelRangeEnd") == Some(start_id) =>
        {
            Some(DeletedInlineSdtEnvelope::Direct)
        }
        [
            XMLNode::Element(incoming_end),
            XMLNode::Element(deletion),
            XMLNode::Element(outgoing_start),
        ] if custom_xml_deletion_range_id(incoming_end, "customXmlDelRangeEnd")
            == Some(start_id)
            && is_w_tag(deletion, "del")
            && custom_xml_deletion_range_id(outgoing_start, "customXmlDelRangeStart")
                .is_some_and(|outgoing_id| {
                    custom_xml_deletion_range_id(end, "customXmlDelRangeEnd") == Some(outgoing_id)
                }) =>
        {
            Some(DeletedInlineSdtEnvelope::WordNormalized)
        }
        _ => None,
    }
}

fn strip_word_normalized_sdt_deletion_boundaries(sdt: &mut Element) {
    let Some(content) = sdt.children.iter_mut().find_map(|child| {
        child
            .as_mut_element()
            .filter(|child| is_w_tag(child, "sdtContent"))
    }) else {
        return;
    };
    content.children.retain(|child| {
        !matches!(child, XMLNode::Element(element)
            if custom_xml_deletion_range_id(element, "customXmlDelRangeEnd").is_some()
                || custom_xml_deletion_range_id(element, "customXmlDelRangeStart").is_some())
    });
}

/// Resolve the structural half of the bounded inline-SDT deletion carrier.
/// The ordinary revision recursion separately resolves the `w:del` inside
/// `w:sdtContent` on Reject. On Accept the whole SDT envelope must disappear,
/// so the carrier is removed atomically before ordinary recursion could leave
/// an empty active content control behind.
fn settle_inline_sdt_deletion_envelopes(
    parent: &mut Element,
    keep_inserted: bool,
    stats: &mut NormalizeStats,
) {
    let mut index = 0usize;
    while index + 2 < parent.children.len() {
        let Some(envelope) = deleted_inline_sdt_envelope_at(&parent.children, index) else {
            index += 1;
            continue;
        };
        if keep_inserted {
            let nested = match &parent.children[index + 1] {
                XMLNode::Element(sdt) => count_nested_revisions(sdt),
                _ => unreachable!("envelope predicate proved an SDT element"),
            };
            parent.children.drain(index..index + 3);
            stats.opaque_resolved += nested;
        } else {
            if envelope == DeletedInlineSdtEnvelope::WordNormalized {
                let XMLNode::Element(sdt) = &mut parent.children[index + 1] else {
                    unreachable!("envelope predicate proved an SDT element")
                };
                strip_word_normalized_sdt_deletion_boundaries(sdt);
            }
            // Remove only the structural range. The SDT shifts into `index`;
            // ordinary reject recursion then unwraps its deleted content.
            parent.children.remove(index + 2);
            parent.children.remove(index);
            index += 1;
        }
    }
}

/// Check if an element is one we "unwrap" (keep children): w:ins, w:moveTo.
fn is_unwrap_revision(element: &Element) -> bool {
    is_w_tag(element, "ins") || is_w_tag(element, "moveTo")
}

/// Check if an element is one we "drop" entirely (remove with children): w:del, w:moveFrom.
fn is_drop_revision(element: &Element) -> bool {
    is_w_tag(element, "del") || is_w_tag(element, "moveFrom")
}

/// Check if an element is an opaque container (w:sdt, w:drawing, etc.)
/// that we must recurse into but not remove.
fn is_opaque_container(element: &Element) -> bool {
    is_w_tag(element, "sdt") || is_w_tag(element, "drawing") || is_w_tag(element, "txbxContent")
}

/// Check if a `w:tr` element has row-level tracking (`w:trPr/w:ins` or `w:trPr/w:del`).
fn has_row_tracking(tr: &Element, tag: &str) -> bool {
    tr.children.iter().any(|c| match c {
        XMLNode::Element(el) if is_w_tag(el, "trPr") => el
            .children
            .iter()
            .any(|tc| matches!(tc, XMLNode::Element(e) if is_w_tag(e, tag))),
        _ => false,
    })
}

/// Check if a `w:tc` element has cell-level tracking (`w:tcPr/w:cellDel`
/// §17.13.5.1 or `w:tcPr/w:cellIns` §17.13.5.2). `tag` is "cellIns" or
/// "cellDel".
fn has_cell_tracking(tc: &Element, tag: &str) -> bool {
    tc.children.iter().any(|c| match c {
        XMLNode::Element(el) if is_w_tag(el, "tcPr") => el
            .children
            .iter()
            .any(|pc| matches!(pc, XMLNode::Element(e) if is_w_tag(e, tag))),
        _ => false,
    })
}

/// Remove `w:cellIns` / `w:cellDel` markers from a kept `w:tc`'s `w:tcPr`,
/// returning how many were removed. Called on cells that SURVIVE a
/// resolution (e.g. an inserted cell on accept): the cell stays, the marker
/// is resolved. Cells that don't survive are dropped wholesale by the
/// caller, so this never silently un-tracks a cell that should have been
/// removed.
fn strip_cell_tracking_markers(tc: &mut Element) -> u32 {
    let mut removed = 0u32;
    for child in tc.children.iter_mut() {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "tcPr")
        {
            let before = el.children.len();
            el.children.retain(
                |pc| !matches!(pc, XMLNode::Element(e) if is_w_tag(e, "cellIns") || is_w_tag(e, "cellDel")),
            );
            removed += (before - el.children.len()) as u32;
        }
    }
    removed
}

/// Check if a `w:tbl` element has at least one `w:tr` child.
///
/// OOXML §17.4.37 requires tables to have a non-zero number of rows.
/// After accept/reject projection drops tracked rows, a table can end up
/// with zero rows. This check lets the projection remove the invalid table
/// rather than emitting spec-invalid XML.
fn has_any_row(tbl: &Element) -> bool {
    tbl.children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(el) if is_w_tag(el, "tr")))
}

// =============================================================================
// Preflight scanning
// =============================================================================

/// Scan a DOCX archive and return a preflight report with revision/comment counts.
///
/// This is a forward, streaming quick-xml scan: it tallies revision/comment
/// element counts directly from `Event::Start`/`Event::Empty` without ever
/// materializing a whole-document `xmltree::Element` tree. Counting is purely a
/// matter of element identity (local name + Word-namespace resolution), so no
/// tree is needed — and building one dominated peak heap on the import path
/// (Rung 6). The produced `PreflightReport` is byte-identical to the previous
/// tree-walking implementation for every corpus document.
pub fn preflight_scan(archive: &DocxArchive) -> Result<PreflightReport, NormalizeError> {
    let part_paths = collect_normalizable_part_paths(archive)?;
    let mut parts = Vec::new();
    let mut totals = PreflightTotals::default();
    let mut warnings = Vec::new();

    for part_path in &part_paths {
        let Some(xml_bytes) = archive.get(part_path) else {
            warnings.push(format!("Referenced part not found in archive: {part_path}"));
            continue;
        };

        let (rev_counts, comment_counts) = scan_part_streaming(xml_bytes)?;

        // Accumulate totals
        totals.revisions.ins += rev_counts.ins;
        totals.revisions.del += rev_counts.del;
        totals.revisions.move_from += rev_counts.move_from;
        totals.revisions.move_to += rev_counts.move_to;
        totals.revisions.del_text += rev_counts.del_text;
        totals.revisions.format_pr_change += rev_counts.format_pr_change;
        totals.comments.anchors += comment_counts.anchors;

        parts.push(PartReport {
            part: part_path.clone(),
            revisions: rev_counts,
            comments: comment_counts,
        });
    }

    // Glossary parts are reported separately so callers can distinguish
    // building-block revisions from ordinary document stories. Full
    // accept/reject resolves them through the same byte kernel.
    let glossary = scan_glossary_parts(archive)?;

    Ok(PreflightReport {
        parts,
        totals,
        glossary,
        warnings,
    })
}

/// One frame of namespace declarations introduced by a single open element.
/// We only store the prefixes *declared on that element* (default namespace uses
/// the empty-string prefix); resolution walks the stack innermost-out. This
/// mirrors the scope handling in `word_xml::parse_document_xml_quick` so that the
/// Word-namespace decision is identical to `is_w_tag` on the built tree.
struct ScanNsFrame {
    bindings: Vec<(String, String)>,
}

/// Resolve whether the element name in `(prefix, scope)` is in the Word namespace,
/// reproducing `is_w_tag`'s rule: a `w:` prefix matches unconditionally, otherwise
/// the prefix (including the default, empty-prefix namespace) must resolve to the
/// Word namespace URI through the in-scope declarations.
fn name_is_word_ns(prefix: Option<&str>, scope: &[ScanNsFrame]) -> bool {
    if prefix == Some("w") {
        return true;
    }
    let lookup = prefix.unwrap_or("");
    for frame in scope.iter().rev() {
        for (p, uri) in &frame.bindings {
            if p == lookup {
                return uri == WORD_NS;
            }
        }
    }
    false
}

/// Classify a single start/empty element by (prefix, local) and bump the matching
/// counter. Mutually exclusive, in the same order as the previous tree walk's
/// `scan_element_recursive` if/else chain so totals stay byte-identical.
fn classify_scan_element(
    prefix: Option<&str>,
    local: &str,
    scope: &[ScanNsFrame],
    rev_counts: &mut PartRevisionCounts,
    comment_counts: &mut PartCommentCounts,
) {
    // Only Word-namespace elements can match any of these tags.
    if !name_is_word_ns(prefix, scope) {
        return;
    }
    match local {
        "ins" => rev_counts.ins += 1,
        "del" => rev_counts.del += 1,
        "moveFrom" => rev_counts.move_from += 1,
        "moveTo" => rev_counts.move_to += 1,
        "delText" => rev_counts.del_text += 1,
        "rPrChange" | "pPrChange" | "tblPrChange" | "tblPrExChange" | "trPrChange"
        | "tcPrChange" | "sectPrChange" => rev_counts.format_pr_change += 1,
        "commentRangeStart" | "commentRangeEnd" | "commentReference" => comment_counts.anchors += 1,
        _ => {}
    }
}

/// Streaming scan of a single story part's XML bytes. Tallies revision/comment
/// counts without building an `xmltree` tree. DOCTYPE is rejected (entity-
/// expansion defense) and element nesting is bounded by `MAX_XML_ELEMENT_DEPTH`,
/// matching the guards on the tree-building parse path.
fn scan_part_streaming(
    bytes: &[u8],
) -> Result<(PartRevisionCounts, PartCommentCounts), NormalizeError> {
    use crate::word_xml::WordXmlError;

    let mut reader = Reader::from_reader(bytes);
    {
        let config = reader.config_mut();
        config.trim_text(false);
        config.expand_empty_elements = false;
        config.check_end_names = true;
    }

    let mut rev_counts = PartRevisionCounts::default();
    let mut comment_counts = PartCommentCounts::default();
    let mut scope: Vec<ScanNsFrame> = Vec::new();
    let mut depth = 0usize;
    let mut buf = Vec::new();

    let to_err = |reader: &Reader<&[u8]>, e: quick_xml::Error| {
        NormalizeError::XmlParseQuick(WordXmlError::QuickXml {
            position: reader.buffer_position(),
            reason: format!("{e}"),
        })
    };

    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| to_err(&reader, e))?;
        match event {
            Event::Start(ref start) => {
                depth += 1;
                if depth > MAX_XML_ELEMENT_DEPTH {
                    return Err(NormalizeError::XmlParseQuick(
                        WordXmlError::XmlDepthExceeded {
                            limit: MAX_XML_ELEMENT_DEPTH,
                            depth,
                        },
                    ));
                }
                let frame = ns_frame_from_start(start, &reader)?;
                scope.push(frame);
                let (prefix, local) = split_name(start.name(), &reader)?;
                classify_scan_element(
                    prefix.as_deref(),
                    &local,
                    &scope,
                    &mut rev_counts,
                    &mut comment_counts,
                );
            }
            Event::Empty(ref start) => {
                let empty_depth = depth + 1;
                if empty_depth > MAX_XML_ELEMENT_DEPTH {
                    return Err(NormalizeError::XmlParseQuick(
                        WordXmlError::XmlDepthExceeded {
                            limit: MAX_XML_ELEMENT_DEPTH,
                            depth: empty_depth,
                        },
                    ));
                }
                // An empty element's own xmlns declarations are in scope for
                // resolving its own name, so push, classify, then pop.
                let frame = ns_frame_from_start(start, &reader)?;
                scope.push(frame);
                let (prefix, local) = split_name(start.name(), &reader)?;
                classify_scan_element(
                    prefix.as_deref(),
                    &local,
                    &scope,
                    &mut rev_counts,
                    &mut comment_counts,
                );
                scope.pop();
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                scope.pop();
            }
            Event::DocType(_) => {
                return Err(NormalizeError::XmlParseQuick(WordXmlError::DoctypeRejected));
            }
            Event::Eof => break,
            // Text / CData / Comment / PI / Decl carry no revision elements.
            _ => {}
        }
        buf.clear();
    }

    Ok((rev_counts, comment_counts))
}

/// Build the namespace-declaration frame for a start/empty tag: harvest only the
/// `xmlns` / `xmlns:foo` attributes (default namespace uses the empty prefix).
fn ns_frame_from_start(
    start: &quick_xml::events::BytesStart<'_>,
    reader: &Reader<&[u8]>,
) -> Result<ScanNsFrame, NormalizeError> {
    use crate::word_xml::WordXmlError;

    let mut bindings: Vec<(String, String)> = Vec::new();
    for attr in start.attributes() {
        let attr = attr.map_err(|e| {
            NormalizeError::XmlParseQuick(WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("malformed attribute: {e}"),
            })
        })?;
        let key = attr.key;
        if key.as_ref() == b"xmlns" {
            let value = decode_attr_value(&attr, reader)?;
            bindings.push((String::new(), value));
            continue;
        }
        let (akey_prefix, akey_local) = split_name(key, reader)?;
        if akey_prefix.as_deref() == Some("xmlns") {
            let value = decode_attr_value(&attr, reader)?;
            bindings.push((akey_local, value));
        }
    }
    Ok(ScanNsFrame { bindings })
}

/// Decode (unescape) a namespace-declaration attribute value to a `String`.
fn decode_attr_value(
    attr: &quick_xml::events::attributes::Attribute<'_>,
    reader: &Reader<&[u8]>,
) -> Result<String, NormalizeError> {
    use crate::word_xml::WordXmlError;
    Ok(attr
        .unescape_value()
        .map_err(|e| {
            NormalizeError::XmlParseQuick(WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("attribute value unescape failed: {e}"),
            })
        })?
        .into_owned())
}

/// Split a `QName` into `(prefix, local)` owned strings.
fn split_name(
    qname: QName<'_>,
    reader: &Reader<&[u8]>,
) -> Result<(Option<String>, String), NormalizeError> {
    use crate::word_xml::WordXmlError;
    let (local, prefix) = qname.decompose();
    let local = std::str::from_utf8(local.into_inner())
        .map_err(|e| {
            NormalizeError::XmlParseQuick(WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("non-UTF-8 element/attribute local name: {e}"),
            })
        })?
        .to_string();
    let prefix = match prefix {
        Some(p) => Some(
            std::str::from_utf8(p.into_inner())
                .map_err(|e| {
                    NormalizeError::XmlParseQuick(WordXmlError::QuickXml {
                        position: reader.buffer_position(),
                        reason: format!("non-UTF-8 namespace prefix: {e}"),
                    })
                })?
                .to_string(),
        ),
        None => None,
    };
    Ok((prefix, local))
}

// =============================================================================
// Final Projection / Normalization
// =============================================================================

/// Normalize a DOCX archive by applying "Final projection": accept all tracked
/// changes at the XML level, producing clean DOCX bytes with zero revision markup.
///
/// Revisions are resolved in every revision-capable story part
/// (`collect_normalizable_part_paths`), including comment bodies — comment
/// paragraphs carry `w:ins`/`w:del` like any other story and Word's Accept All
/// resolves them too. Comment *identity* (author, date, anchors) is preserved;
/// only revision markup inside comment text is resolved.
pub fn normalize_docx(
    archive: &DocxArchive,
) -> Result<(DocxArchive, NormalizationResult), NormalizeError> {
    refuse_malformed_cell_merges(archive)?;
    accept_all_parts(archive)
}

/// [`normalize_docx`] without the unmodeled-revision refusal. Only the
/// serializer's accepted BASE image uses it (`normalize_if_needed`): that
/// image is a template for untouched parts, never handed out as a resolved
/// document, and an unmodeled change in it rides along verbatim as today.
fn accept_all_parts(
    archive: &DocxArchive,
) -> Result<(DocxArchive, NormalizationResult), NormalizeError> {
    let mut output = archive.clone();
    let mut result = NormalizationResult::default();

    let mut part_paths = collect_normalizable_part_paths(archive)?;
    part_paths.extend(collect_glossary_part_paths(archive)?);

    for part_path in &part_paths {
        let Some(xml_bytes) = output.get(part_path) else {
            continue;
        };

        let mut root = parse_xml(xml_bytes)?;
        let mut stats = NormalizeStats::default();

        drop_bookmarks_of_move_sources(&mut root);
        let range_markers_before = capture_tree_range_markers(&root);
        let auxiliary_resolved = resolve_table_grid_history(&mut root, true, part_path)?;
        normalize_children(&mut root, &mut stats, false);
        collapse_torn_range_markers_in_tree(&mut root, &range_markers_before);

        let new_bytes = write_xml(&root)?;
        // Safe to unwrap: we just read this part, so it exists.
        output.upsert(part_path, new_bytes);

        if stats.revisions_resolved > 0 || stats.opaque_resolved > 0 || auxiliary_resolved > 0 {
            result.parts_normalized.push(part_path.clone());
        }
        result.revisions_resolved += stats.revisions_resolved;
        result.opaque_nodes_resolved_revisions_count += stats.opaque_resolved;
    }

    for part in remove_comments_that_lost_their_anchor(&mut output, archive, &part_paths)?
        .into_iter()
        .chain(prune_unreferenced_note_definitions(
            &mut output,
            archive,
            &part_paths,
        )?)
    {
        if !result.parts_normalized.contains(&part) {
            result.parts_normalized.push(part);
        }
    }

    result.unresolved_glossary_revisions = 0;

    Ok((output, result))
}

// =============================================================================
// Reject All (inverse of normalize/accept)
// =============================================================================

/// Reject all tracked changes in a DOCX archive: produce the "before" version.
///
/// Inverse of `normalize_docx()` (accept all):
/// - w:ins / w:moveTo → **drop** entirely (additions being rejected)
/// - w:del / w:moveFrom → **unwrap** (deletions being restored)
/// - w:delText → **convert to w:t** (restore deleted text)
/// - *PrChange → **restore the previous properties** the record carries
///   (§17.13.5.29–.32; accept keeps the new properties and drops the record)
pub fn reject_all_docx(
    archive: &DocxArchive,
) -> Result<(DocxArchive, NormalizationResult), NormalizeError> {
    refuse_malformed_cell_merges(archive)?;
    let mut output = archive.clone();
    let mut result = NormalizationResult::default();

    let mut part_paths = collect_normalizable_part_paths(archive)?;
    part_paths.extend(collect_glossary_part_paths(archive)?);

    for part_path in &part_paths {
        let Some(xml_bytes) = output.get(part_path) else {
            continue;
        };

        let mut root = parse_xml(xml_bytes)?;
        let mut stats = NormalizeStats::default();

        untrack_reference_only_insertions(&mut root);
        let range_markers_before = capture_tree_range_markers(&root);
        let auxiliary_resolved = resolve_table_grid_history(&mut root, false, part_path)?;
        reject_children(&mut root, &mut stats, false);
        collapse_torn_range_markers_in_tree(&mut root, &range_markers_before);

        let new_bytes = write_xml(&root)?;
        output.upsert(part_path, new_bytes);

        if stats.revisions_resolved > 0 || stats.opaque_resolved > 0 || auxiliary_resolved > 0 {
            result.parts_normalized.push(part_path.clone());
        }
        result.revisions_resolved += stats.revisions_resolved;
        result.opaque_nodes_resolved_revisions_count += stats.opaque_resolved;
    }

    for part in remove_comments_that_lost_their_anchor(&mut output, archive, &part_paths)?
        .into_iter()
        .chain(prune_unreferenced_note_definitions(
            &mut output,
            archive,
            &part_paths,
        )?)
    {
        if !result.parts_normalized.contains(&part) {
            result.parts_normalized.push(part);
        }
    }

    result.unresolved_glossary_revisions = 0;

    Ok((output, result))
}

/// Resolve only the opaque glossary story set.
///
/// The model projection has already resolved every modeled document story
/// before calling this helper. Restricting the byte-kernel pass to glossary
/// parts preserves the established document scaffold byte-for-byte while
/// closing the otherwise-unmodeled glossary coverage gap.
pub(crate) fn resolve_glossary_docx(
    archive: &DocxArchive,
    accept: bool,
) -> Result<DocxArchive, NormalizeError> {
    let mut output = archive.clone();
    for part_path in collect_glossary_part_paths(archive)? {
        let Some(xml_bytes) = output.get(&part_path) else {
            continue;
        };
        let mut root = parse_xml(xml_bytes)?;
        let mut stats = NormalizeStats::default();
        let range_markers_before = capture_tree_range_markers(&root);
        resolve_table_grid_history(&mut root, accept, &part_path)?;
        if accept {
            normalize_children(&mut root, &mut stats, false);
        } else {
            reject_children(&mut root, &mut stats, false);
        }
        collapse_torn_range_markers_in_tree(&mut root, &range_markers_before);
        output.upsert(&part_path, write_xml(&root)?);
    }
    Ok(output)
}

/// Outcome of [`resolve_opaque_fragment_revisions`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FragmentResolution {
    /// A revision was resolved; the new bytes are the re-serialized fragment.
    /// The caller recomputes the opaque's `content_hash`.
    Resolved(Vec<u8>),
    /// The fragment carried no revision to resolve, so it is left byte-verbatim
    /// (clean opaque — the round-trip-stays-verbatim invariant). This covers
    /// both a parsed fragment with no revisions AND an unparseable fragment that
    /// contains no revision markers (nothing to resolve either way).
    Clean,
    /// The fragment failed XML parsing or active-branch selection, BUT its raw bytes contain a revision marker
    /// (`<w:ins`/`<w:del`/`w:moveFrom`/`w:moveTo`). We cannot resolve revisions
    /// we cannot parse — and silently leaving them is the exact silent-fallback
    /// this descent exists to kill — so the projection must REFUSE rather than
    /// emit a document that still carries pending revisions it claimed to resolve.
    UnparseableWithRevisions,
    /// The XML parses, but its auxiliary table history cannot be safely
    /// resolved. Full projection must refuse with this diagnostic.
    InvalidTableHistory(String),
}

/// Resolve all tracked changes inside an opaque inline's `raw_xml` fragment,
/// reusing the byte-path resolver that already descends uniformly into opaque
/// content (`reject_children` / `normalize_children` recurse into "everything
/// else", so any container — textbox `txbxContent`, content-control
/// `sdtContent`, fldSimple result, inline customXml/smartTag/ruby — that
/// legally carries `w:ins`/`w:del` is resolved).
///
/// This is the IR projection's entry into the byte resolver: the typed
/// accept/reject in `tracked_model.rs` freezes these interiors as opaque bytes,
/// so revisions inside them are invisible to the segment-level filtering. This
/// function reparses the fragment with the same pair the opaque-mutation verbs
/// use (`parse_raw_fragment` / `serialize_raw_fragment`), runs `normalize_children`
/// (accept) or `reject_children` (reject) with `inside_opaque = true`, and
/// re-serializes.
///
/// Returns [`FragmentResolution::Resolved`] only when a revision was actually
/// resolved; [`FragmentResolution::Clean`] when there was nothing to resolve
/// (clean opaque, left byte-verbatim); and
/// [`FragmentResolution::UnparseableWithRevisions`] when the fragment cannot be
/// parsed yet its bytes carry a revision marker — the caller must then refuse
/// (no silent skip of revisions we promised to resolve).
pub(crate) fn resolve_opaque_fragment_revisions(
    raw: &[u8],
    keep_inserted: bool,
) -> FragmentResolution {
    let mut fragment = match crate::word_xml::OpaqueFragment::parse(raw) {
        Ok(fragment) => fragment,
        Err(_) => {
            // Can't parse it. If the raw bytes carry a revision marker, the
            // descent would silently leave a pending revision unresolved —
            // refuse instead (mirrors the quarantine guard's "revisions
            // invisible to this operation" precedent). No markers → nothing to
            // resolve, leave verbatim.
            if REVISION_BYTE_MARKERS.iter().any(|m| memchr_find(raw, m))
                || memchr_find(raw, b"tblGridChange")
            {
                return FragmentResolution::UnparseableWithRevisions;
            }
            return FragmentResolution::Clean;
        }
    };
    // Every representation of a compatibility drawing shows the same shape, so
    // resolving everything resolves each of them, as the package resolver does.
    match resolve_opaque_children(fragment.envelope_mut(), keep_inserted, "opaque fragment") {
        Ok(false) => FragmentResolution::Clean,
        Ok(true) => FragmentResolution::Resolved(fragment.serialize()),
        Err(error) => FragmentResolution::InvalidTableHistory(format!("{error:?}")),
    }
}

/// Resolve every tracked change below `container`, an element wrapping
/// verbatim-preserved content, with the package resolver's own logic. Returns
/// whether anything was resolved. The container itself is never a revision: it
/// is the envelope an opaque fragment parses into, or one the caller builds.
pub(crate) fn resolve_opaque_children(
    container: &mut Element,
    keep_inserted: bool,
    part: &str,
) -> Result<bool, NormalizeError> {
    let mut stats = NormalizeStats::default();
    let auxiliary_resolved = resolve_table_grid_history(container, keep_inserted, part)?;
    // The container IS the opaque container, so descend with
    // `inside_opaque = true` from the start — its inner revisions count in the
    // opaque bucket, exactly as the whole-document path tallies them when it
    // recurses through `is_opaque_container`.
    if keep_inserted {
        normalize_children(container, &mut stats, true);
    } else {
        reject_children(container, &mut stats, true);
    }
    Ok(stats.opaque_resolved != 0 || stats.revisions_resolved != 0 || auxiliary_resolved != 0)
}

/// Resolve ONLY the interior revisions whose `w:id` is in `selected`
/// (RFC-0002 §Phase-3b selective descent) — the by-id twin of
/// [`resolve_opaque_fragment_revisions`], which is all-or-nothing. `accept` picks
/// direction: accept keeps an insertion / drops a deletion; reject drops an
/// insertion / restores a deletion (`w:delText`→`w:t`). Every carrier NOT in
/// `selected` is left byte-verbatim, so a document can carry a mix of resolved and
/// still-pending interior revisions.
///
/// Only a TOP-LEVEL `w:ins`/`w:del` is id-resolvable, mirroring
/// `tracked_model::visit_fragment_carriers` (the classifier that decides which
/// ids are selectable): moves are pair-carriers (resolving one half by id
/// would orphan its counterpart and range markers), `*PrChange` is formatting,
/// and a stacked carrier (inside another carrier) is never individually
/// addressable. The caller passes only ids the classifier proved selectable;
/// the matching rules here enforce the same shape so the two can't diverge.
pub(crate) fn resolve_fragment_selected(
    raw: &[u8],
    selected: &std::collections::HashSet<u32>,
    accept: bool,
) -> FragmentResolution {
    let mut fragment = match crate::word_xml::OpaqueFragment::parse(raw) {
        Ok(fragment) => fragment,
        Err(_) => {
            if REVISION_BYTE_MARKERS.iter().any(|m| memchr_find(raw, m)) {
                return FragmentResolution::UnparseableWithRevisions;
            }
            return FragmentResolution::Clean;
        }
    };
    let mut resolved = 0usize;
    resolve_selected_in(fragment.active_mut(), selected, accept, &mut resolved);
    if resolved == 0 {
        FragmentResolution::Clean
    } else {
        FragmentResolution::Resolved(fragment.serialize())
    }
}

fn selected_wid(el: &Element, selected: &std::collections::HashSet<u32>) -> bool {
    crate::xml_attrs::attr_get(el, "w:id")
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|id| selected.contains(&id))
}

/// Walk `parent`'s children, resolving a selected TOP-LEVEL `w:ins`/`w:del`
/// carrier by drop/unwrap (per `accept`) and leaving every other carrier
/// verbatim. Matching never descends INTO a carrier: a stacked revision is not
/// individually addressable (the classifier reports it census-only), so a
/// selected id that happens to match a stacked carrier's id must not mutate it.
fn resolve_selected_in(
    parent: &mut Element,
    selected: &std::collections::HashSet<u32>,
    accept: bool,
    resolved: &mut usize,
) {
    let mut new_children: Vec<XMLNode> = Vec::with_capacity(parent.children.len());
    for child in parent.children.drain(..) {
        match child {
            XMLNode::Element(mut el)
                if (is_w_tag(&el, "ins") || is_w_tag(&el, "del"))
                    && selected_wid(&el, selected) =>
            {
                *resolved += 1;
                let is_addition = is_w_tag(&el, "ins");
                // accept+addition or reject+deletion → keep the wrapped content;
                // otherwise the whole carrier is dropped.
                let keep_children = accept == is_addition;
                if keep_children {
                    if !is_addition {
                        // Restoring a deletion: w:delText → w:t before promoting.
                        let mut stats = NormalizeStats::default();
                        convert_del_text_to_t(&mut el, &mut stats, true);
                    }
                    // Promote the children VERBATIM: any carrier nested in them
                    // was stacked, is census-only by the classifier, and stays
                    // pending markup.
                    new_children.extend(el.children);
                }
                // else: dropped entirely
            }
            XMLNode::Element(mut el) => {
                // Recurse through ordinary wrappers (paragraph, hyperlink,
                // smartTag, …) but never INTO a revision carrier — its interior
                // is stacked territory.
                let had_nested_revision = has_nested_content_revision(&el);
                if !is_content_revision(&el) {
                    resolve_selected_in(&mut el, selected, accept, resolved);
                }
                if !is_revision_emptied_hyperlink(&el, had_nested_revision) {
                    new_children.push(XMLNode::Element(el));
                }
            }
            other => new_children.push(other),
        }
    }
    parent.children = new_children;
}

/// Reject children of an element in-place (inverse of `normalize_children`).
///
/// For each child:
/// - w:ins / w:moveTo → drop entirely (reject additions)
/// - w:del / w:moveFrom → unwrap (restore deletions)
/// - w:delText → convert to w:t (restore deleted text)
/// - *PrChange → restore previous properties from the record
/// - Everything else → recurse
fn reject_children(parent: &mut Element, stats: &mut NormalizeStats, inside_opaque: bool) {
    settle_inline_sdt_deletion_envelopes(parent, false, stats);
    // Join paragraphs whose mark insertion this reject un-proposes (must
    // precede the revision pass, which drops the markers).
    join_mark_resolved_paragraphs(parent, /*keep_inserted=*/ false, stats);

    // First pass: reject *PrChange records — restore the previous
    // properties they carry (§17.13.5.29–.32; accept drops the record and
    // keeps the new properties instead).
    reject_pr_changes(parent, stats, inside_opaque);

    // Second pass: handle content revisions with inverted logic.
    let mut new_children: Vec<XMLNode> = Vec::with_capacity(parent.children.len());
    // Where a carrier was dropped or unwrapped: the runs that meet there may
    // be the pieces of one run an edit split.
    let mut seams: Vec<usize> = Vec::new();

    // Range markers the dropped carriers hold, kept where they were.
    let mut removed_stretch: Vec<Element> = Vec::new();

    for child in parent.children.drain(..) {
        match &child {
            XMLNode::Element(el) if is_w_tag(el, "ins") || is_w_tag(el, "moveTo") => {
                collect_tree_range_markers(el, &mut removed_stretch);
            }
            XMLNode::Element(_) => {
                keep_removed_stretch_markers(&mut removed_stretch, &mut new_children)
            }
            _ => {}
        }
        match child {
            XMLNode::Element(el) if is_w_tag(&el, "ins") || is_w_tag(&el, "moveTo") => {
                // w:ins, w:moveTo → drop entirely (reject additions)
                seams.push(new_children.len());
                let count = 1 + count_nested_revisions(&el);
                if inside_opaque {
                    stats.opaque_resolved += count;
                } else {
                    stats.revisions_resolved += count;
                }
            }
            XMLNode::Element(mut el) if is_w_tag(&el, "del") || is_w_tag(&el, "moveFrom") => {
                // w:del, w:moveFrom → unwrap: keep children, drop the wrapper
                if inside_opaque {
                    stats.opaque_resolved += 1;
                } else {
                    stats.revisions_resolved += 1;
                }
                // Convert w:delText → w:t inside these children before promoting
                convert_del_text_to_t(&mut el, stats, inside_opaque);
                if is_w_tag(&el, "moveFrom") {
                    // Word's reachable "insert paragraph, then move it" shape
                    // serializes the earlier insertion inside moveFrom at the
                    // source. Rejecting the move restores that source content
                    // and settles the nested insertion as normal content; a
                    // generic recursive reject would wrongly delete it.
                    unwrap_insertions_restored_with_move_from(&mut el, stats, inside_opaque);
                }
                // Recursively reject children before promoting them
                reject_children(&mut el, stats, inside_opaque);
                // Promote all children of the revision wrapper
                seams.push(new_children.len());
                new_children.extend(el.children);
                seams.push(new_children.len());
            }
            XMLNode::Element(el) if is_move_range_marker(&el) => {
                // Move range delimiters go with the move they bracket
                // (reject un-proposed the move: moveFrom content restored,
                // moveTo dropped — see is_move_range_marker).
                if inside_opaque {
                    stats.opaque_resolved += 1;
                } else {
                    stats.revisions_resolved += 1;
                }
            }
            XMLNode::Element(el) if is_w_tag(&el, "tr") && !row_survives(&el, false) => {
                // Row-level tracked insertion (w:trPr/w:ins): drop the entire
                // row on reject. The row was added as a tracked change — undoing
                // that means removing it entirely. A row whose every cell is a
                // w:cellIns goes too (`resolution_rules::row_survives_resolution`).
                let count = dropped_row_revision_count(&el, "ins");
                if inside_opaque {
                    stats.opaque_resolved += count;
                } else {
                    stats.revisions_resolved += count;
                }
            }
            XMLNode::Element(el) if is_w_tag(&el, "tc") && has_cell_tracking(&el, "cellIns") => {
                // Cell-level tracked insertion (w:tcPr/w:cellIns, §17.13.5.2):
                // drop the entire cell on reject — the cell was added as a
                // tracked change, undoing that removes it. Mirrors the IR
                // reject path (tracked_model.rs). A STACKED cell (cellIns +
                // cellDel) also lands here: it drops in both full resolutions.
                let count = 1 + count_nested_revisions(&el);
                if inside_opaque {
                    stats.opaque_resolved += count;
                } else {
                    stats.revisions_resolved += count;
                }
            }
            XMLNode::Element(mut el) => {
                // Non-revision element: recurse into it
                let had_nested_revision = has_nested_content_revision(&el);
                let child_opaque = inside_opaque || is_opaque_container(&el);
                let cell_merge = if is_w_tag(&el, "tc") {
                    take_cell_merge(&mut el, false)
                } else {
                    None
                };
                reject_children(&mut el, stats, child_opaque);
                // A surviving cell's w:cellDel marker (§17.13.5.1) is resolved
                // on reject: the cell is restored, the marker goes.
                if is_w_tag(&el, "tc") {
                    let mut stripped = strip_cell_tracking_markers(&mut el);
                    if let Some(merge) = cell_merge {
                        apply_cell_merge(&mut el, merge);
                        stripped += 1;
                    }
                    if inside_opaque {
                        stats.opaque_resolved += stripped;
                    } else {
                        stats.revisions_resolved += stripped;
                    }
                }
                if is_w_tag(&el, "tbl") {
                    settle_resolved_cell_merges(&mut el);
                }
                // OOXML §17.4.37: a table must contain a non-zero number of rows.
                // After rejection drops all inserted rows, a table can end up with
                // zero w:tr children. Remove it rather than producing invalid XML.
                if is_w_tag(&el, "tbl") && !has_any_row(&el) {
                    // Table is now empty — drop it entirely.
                } else if is_revision_emptied_hyperlink(&el, had_nested_revision) {
                    // A paragraph-level hyperlink carries revisions on its inner
                    // runs. Resolving away every run removes the hyperlink too;
                    // retaining an empty relationship-bearing shell would invent
                    // a semantic object absent from this terminal.
                } else {
                    new_children.push(XMLNode::Element(el));
                }
            }
            other => {
                new_children.push(other);
            }
        }
    }
    keep_removed_stretch_markers(&mut removed_stretch, &mut new_children);

    if inside_opaque {
        rejoin_runs_at_seams(&mut new_children, &seams);
    }
    parent.children = new_children;
}

fn unwrap_insertions_restored_with_move_from(
    parent: &mut Element,
    stats: &mut NormalizeStats,
    inside_opaque: bool,
) {
    let mut children = Vec::with_capacity(parent.children.len());
    for child in parent.children.drain(..) {
        match child {
            XMLNode::Element(mut insertion) if is_w_tag(&insertion, "ins") => {
                if inside_opaque {
                    stats.opaque_resolved += 1;
                } else {
                    stats.revisions_resolved += 1;
                }
                children.append(&mut insertion.children);
            }
            XMLNode::Element(mut element) => {
                unwrap_insertions_restored_with_move_from(&mut element, stats, inside_opaque);
                children.push(XMLNode::Element(element));
            }
            other => children.push(other),
        }
    }
    parent.children = children;
}

/// Convert `w:delText` → `w:t` and `w:delInstrText` → `w:instrText`
/// in-place, recursively. Used during reject to restore deleted content
/// as normal content — both element types are only valid inside `w:del`.
fn convert_del_text_to_t(element: &mut Element, stats: &mut NormalizeStats, inside_opaque: bool) {
    for child in element.children.iter_mut() {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        // delText → t, delInstrText → instrText (the deleted↔plain run-content
        // pairs — the single source of truth in DELETED_RUN_CONTENT_PAIRS).
        if let Some(plain_local) = plain_run_content_name(local_element_name(el)) {
            rename_local_preserving_prefix(el, plain_local);
            if inside_opaque {
                stats.opaque_resolved += 1;
            } else {
                stats.revisions_resolved += 1;
            }
        } else {
            let child_opaque = inside_opaque || is_opaque_container(el);
            convert_del_text_to_t(el, stats, child_opaque);
        }
    }
}

/// Rename an element to a new local name, preserving its existing namespace
/// prefix (`w:delText` → `w:t`, bare `delText` → `t`).
fn rename_local_preserving_prefix(el: &mut Element, new_local: &str) {
    if let Some(colon) = el.name.find(':') {
        el.name = format!("{}:{new_local}", &el.name[..colon]);
    } else {
        el.name = new_local.to_string();
    }
}

// ============================================================================
// Torn range-marker collapse (archive / raw-XML resolution path)
// ============================================================================
//
// The raw-XML twin of `tracked_model::collapse_resolution_torn_range_markers`
// (see there for the full domain rule). A bookmark / comment range / permission
// range is a start/end pair joined by a part-local id. A dropped revision
// carrier keeps the range markers it holds at the removal point
// (`keep_removed_stretch_markers`), so the pair stays whole; a pair wholly
// inside one removed stretch goes with it. A removal that still tears a pair
// (a dropped row or cell) would leave it schema-invalid (Word repairs on
// open), so rather than drop the survivor too (deleting base content no
// revision proposed removing), we re-insert the removed half adjacent to the
// survivor, collapsing the range to a point. Only pairs WHOLE before this
// resolution are repaired; a half already lone in the input is the document's
// own state and passes through untouched.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum RangeFamilyTag {
    Bookmark,
    Comment,
    Permission,
}

type TreeRangeKey = (RangeFamilyTag, String);

/// Classify an element as one half of a paired range marker:
/// `(family, part-local id, is_start)`.
fn tree_range_marker_of(el: &Element) -> Option<(RangeFamilyTag, String, bool)> {
    let (family, is_start) = match local_element_name(el) {
        "bookmarkStart" => (RangeFamilyTag::Bookmark, true),
        "bookmarkEnd" => (RangeFamilyTag::Bookmark, false),
        "commentRangeStart" => (RangeFamilyTag::Comment, true),
        "commentRangeEnd" => (RangeFamilyTag::Comment, false),
        "permStart" => (RangeFamilyTag::Permission, true),
        "permEnd" => (RangeFamilyTag::Permission, false),
        _ => return None,
    };
    let id = crate::xml_attrs::attr_get(el, "id")?.clone();
    Some((family, id, is_start))
}

/// The captured start/end elements of a range pair (pre-resolution), so a
/// dropped half can be re-materialized adjacent to the surviving half.
#[derive(Default)]
struct TreeRangeCapture {
    start: Option<Element>,
    end: Option<Element>,
}

/// Snapshot every paired range marker in a part tree.
fn capture_tree_range_markers(
    root: &Element,
) -> std::collections::HashMap<TreeRangeKey, TreeRangeCapture> {
    fn walk(el: &Element, map: &mut std::collections::HashMap<TreeRangeKey, TreeRangeCapture>) {
        if let Some((family, id, is_start)) = tree_range_marker_of(el) {
            let slot = map.entry((family, id)).or_default();
            if is_start {
                slot.start = Some(el.clone());
            } else {
                slot.end = Some(el.clone());
            }
        }
        for child in &el.children {
            if let XMLNode::Element(c) = child {
                walk(c, map);
            }
        }
    }
    let mut map = std::collections::HashMap::new();
    walk(root, &mut map);
    map
}

/// Presence of each pair's start/end after resolution: `(start, end)`.
fn survivor_tree_range_markers(
    root: &Element,
) -> std::collections::HashMap<TreeRangeKey, (bool, bool)> {
    fn walk(el: &Element, map: &mut std::collections::HashMap<TreeRangeKey, (bool, bool)>) {
        if let Some((family, id, is_start)) = tree_range_marker_of(el) {
            let slot = map.entry((family, id)).or_insert((false, false));
            if is_start {
                slot.0 = true;
            } else {
                slot.1 = true;
            }
        }
        for child in &el.children {
            if let XMLNode::Element(c) = child {
                walk(c, map);
            }
        }
    }
    let mut map = std::collections::HashMap::new();
    walk(root, &mut map);
    map
}

/// Collect, in document order, the paired range markers inside `el`, a
/// revision carrier the resolution drops.
fn collect_tree_range_markers(el: &Element, out: &mut Vec<Element>) {
    for child in &el.children {
        if let XMLNode::Element(c) = child {
            if tree_range_marker_of(c).is_some() {
                out.push(c.clone());
            } else {
                collect_tree_range_markers(c, out);
            }
        }
    }
}

/// Keep the range markers of a stretch of consecutive dropped carriers at the
/// removal point, except a pair lying wholly within the stretch, which goes
/// with it (ECMA-376 §17.13.6; the model twin is
/// `tracked_model::retain_segments_keeping_range_markers`).
fn keep_removed_stretch_markers(stretch: &mut Vec<Element>, kept: &mut Vec<XMLNode>) {
    if stretch.is_empty() {
        return;
    }
    let key_of = |el: &Element| {
        let (family, id, is_start) =
            tree_range_marker_of(el).expect("stretch holds only range markers");
        ((family, id), is_start)
    };
    let mut started = std::collections::HashSet::new();
    let mut whole = std::collections::HashSet::new();
    for el in stretch.iter() {
        let (key, is_start) = key_of(el);
        if is_start {
            started.insert(key);
        } else if started.contains(&key) {
            whole.insert(key);
        }
    }
    kept.extend(
        stretch
            .drain(..)
            .filter(|el| !whole.contains(&key_of(el).0))
            .map(XMLNode::Element),
    );
}

/// Re-pair every range marker the resolution just applied to `root` tore.
fn collapse_torn_range_markers_in_tree(
    root: &mut Element,
    before: &std::collections::HashMap<TreeRangeKey, TreeRangeCapture>,
) {
    if before.is_empty() {
        return;
    }
    let survivors = survivor_tree_range_markers(root);
    for (key, capture) in before {
        let (Some(start_el), Some(end_el)) = (&capture.start, &capture.end) else {
            continue;
        };
        let (start_now, end_now) = survivors.get(key).copied().unwrap_or((false, false));
        // Both survive → shrank but paired. Both gone → whole range removed with
        // resolved content. Neither is a tear.
        if start_now == end_now {
            continue;
        }
        let (survivor_is_start, partner) = if start_now {
            (true, end_el.clone())
        } else {
            (false, start_el.clone())
        };
        insert_tree_partner_adjacent(root, key, survivor_is_start, partner);
    }
}

/// Insert `partner` immediately adjacent to the surviving half of `key` (after a
/// surviving start, before a surviving end). Stops at the first survivor found.
fn insert_tree_partner_adjacent(
    el: &mut Element,
    key: &TreeRangeKey,
    survivor_is_start: bool,
    partner: Element,
) -> bool {
    let mut survivor_at: Option<usize> = None;
    for (i, child) in el.children.iter().enumerate() {
        if let XMLNode::Element(c) = child
            && let Some((family, id, is_start)) = tree_range_marker_of(c)
            && family == key.0
            && id == key.1
            && is_start == survivor_is_start
        {
            survivor_at = Some(i);
            break;
        }
    }
    if let Some(i) = survivor_at {
        let at = if survivor_is_start { i + 1 } else { i };
        el.children.insert(at, XMLNode::Element(partner));
        return true;
    }
    for child in el.children.iter_mut() {
        if let XMLNode::Element(c) = child
            && insert_tree_partner_adjacent(c, key, survivor_is_start, partner.clone())
        {
            return true;
        }
    }
    false
}

// =============================================================================
// Normalize-if-needed
// =============================================================================

/// Normalize a DOCX archive if it contains pre-existing tracked changes.
///
/// Byte-level markers that indicate tracked-change revision markup in
/// WordprocessingML XML. If none of these appear in the raw bytes of any
/// story part, the document is clean and normalization can be skipped entirely.
///
/// These cover the same tags that `scan_element_recursive` and
/// `normalize_children` handle: ins, del, moveFrom, moveTo, delText,
/// and all *PrChange variants.
const REVISION_BYTE_MARKERS: &[&[u8]] = &[
    b"<w:ins ",
    b"<w:ins>",
    b"<w:del ",
    b"<w:del>",
    b"<w:moveFrom ",
    b"<w:moveFrom>",
    b"<w:moveTo ",
    b"<w:moveTo>",
    b"<w:delText",
    b"<w:rPrChange",
    b"<w:pPrChange",
    b"<w:tblPrChange",
    b"<w:tblPrExChange",
    b"<w:trPrChange",
    b"<w:tcPrChange",
    b"<w:sectPrChange",
    b"<w:cellMerge",
];

/// The SAME tracked-change carrier inventory as [`REVISION_BYTE_MARKERS`],
/// expressed as element LOCAL names for a parsed-tree walk instead of raw-byte
/// substrings. One source of truth, two forms: the byte list is the fast gate
/// (`has_revision_markup_bytes`), this is what a tree walk over opaque interior
/// content matches against (`tracked_model::enumerate_opaque_interior_revisions`).
/// `opaque_interior_inventory_mirrors_byte_markers` (below) pins the two so a
/// tag added to one but not the other fails loudly.
///
/// `delText` appears in the byte list but NOT here: it is deleted-text CONTENT
/// inside a `w:del`, not a revision carrier of its own — a tree walk that
/// counted it would double-count the enclosing `w:del`.
pub(crate) const REVISION_ELEMENT_LOCAL_NAMES: &[&str] = &[
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "rPrChange",
    "pPrChange",
    "tblPrChange",
    "tblPrExChange",
    "trPrChange",
    "tcPrChange",
    "sectPrChange",
    "cellMerge",
];

/// Fast byte-level check: does this single XML fragment (e.g. an opaque
/// wrapper's preserved `raw_xml`) contain any tracked-change revision markup?
/// Reuses [`REVISION_BYTE_MARKERS`] — the one carrier inventory — so a caller
/// scanning opaque interior bytes shares the exact tag set the normalizer
/// resolves, and the two cannot drift.
pub(crate) fn has_revision_markup_bytes(bytes: &[u8]) -> bool {
    REVISION_BYTE_MARKERS
        .iter()
        .any(|marker| memchr_find(bytes, marker))
}

/// Fast byte-level check: does any story part in the archive contain revision
/// markup? This avoids the expensive xmltree parse that `preflight_scan` uses.
pub(crate) fn has_revision_markup_fast(archive: &DocxArchive) -> bool {
    for entry_name in archive.list() {
        // Only check WordprocessingML story parts under word/
        let dominated = entry_name.starts_with("word/") && entry_name.ends_with(".xml");
        if !dominated {
            continue;
        }
        // Skip relationships, settings, styles, etc. — only check parts that
        // can contain content revisions.
        if entry_name.contains("/_rels/")
            || entry_name.contains("/theme/")
            || entry_name == "word/settings.xml"
            || entry_name == "word/styles.xml"
            || entry_name == "word/numbering.xml"
            || entry_name == "word/fontTable.xml"
            || entry_name == "word/webSettings.xml"
        {
            continue;
        }

        if let Some(bytes) = archive.get(entry_name) {
            // Auxiliary geometry is not a revision identity, but terminal
            // normalization must still remove it on an otherwise clean file.
            if memchr_find(bytes, b"tblGridChange") {
                return true;
            }
            for marker in REVISION_BYTE_MARKERS {
                if memchr_find(bytes, marker) {
                    return true;
                }
            }
        }
    }
    false
}

/// Simple byte substring search (uses a window scan).
fn memchr_find(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Runs a fast byte-level scan first; if any revision markup is found, accepts
/// all changes via `normalize_docx`. Returns the (possibly normalized) archive.
/// Clean documents pass through unchanged.
pub fn normalize_if_needed(archive: &DocxArchive) -> Result<DocxArchive, NormalizeError> {
    if has_revision_markup_fast(archive) {
        // Serialization needs the accepted image of modeled document stories
        // as its base, but the caller's requested projection decides the
        // glossary direction later. Preserve glossary bytes here so a RejectAll
        // projection is not irreversibly pre-accepted before its final pass.
        let glossary_parts: Vec<(String, Vec<u8>)> = collect_glossary_part_paths(archive)?
            .into_iter()
            .filter_map(|path| archive.get(&path).map(|bytes| (path, bytes.to_vec())))
            .collect();
        let (mut normalized, _) = accept_all_parts(archive)?;
        for (path, bytes) in glossary_parts {
            normalized.upsert(&path, bytes);
        }
        Ok(normalized)
    } else {
        Ok(archive.clone())
    }
}

#[derive(Default)]
struct NormalizeStats {
    revisions_resolved: u32,
    opaque_resolved: u32,
}

/// Normalize children of an element in-place.
///
/// This is the core recursive transformation. For each child:
/// - w:ins / w:moveTo → unwrap (replace element with its children)
/// - w:del / w:moveFrom → drop entirely
/// - w:delText → drop
/// - *PrChange → remove from parent properties element
/// - Everything else → recurse
///
/// `inside_opaque` tracks whether we're inside an opaque container (w:sdt, etc.)
/// so we can count those separately.
/// Which paragraph-mark tracked markers (`w:pPr/w:rPr`) a paragraph carries,
/// as `(insertion_class, deletion_class)`. A moved paragraph's pilcrow carries
/// `w:moveTo` / `w:moveFrom` (the moved-paragraph twin of `w:ins` / `w:del`),
/// and for paragraph-break RESOLUTION it behaves identically: `w:moveTo` is an
/// insertion-class mark (the break is removed on REJECT), `w:moveFrom` a
/// deletion-class mark (the break is removed on ACCEPT). This mirrors the model,
/// whose para-mark status is `Inserted` for a moveTo pilcrow and `Deleted` for a
/// moveFrom pilcrow (`import`'s `extract_para_mark_status`) — so the wire join
/// below merges exactly the same paragraphs the model's
/// `merge_marked_paragraphs_*` does.
fn para_mark_markers(p: &Element) -> (bool, bool) {
    let Some(ppr) = p.children.iter().find_map(|c| match c {
        XMLNode::Element(el) if is_w_tag(el, "pPr") => Some(el),
        _ => None,
    }) else {
        return (false, false);
    };
    let Some(rpr) = ppr.children.iter().find_map(|c| match c {
        XMLNode::Element(el) if is_w_tag(el, "rPr") => Some(el),
        _ => None,
    }) else {
        return (false, false);
    };
    let mut has_ins = false;
    let mut has_del = false;
    for c in &rpr.children {
        if let XMLNode::Element(el) = c {
            if is_w_tag(el, "ins") || is_w_tag(el, "moveTo") {
                has_ins = true;
            }
            if is_w_tag(el, "del") || is_w_tag(el, "moveFrom") {
                has_del = true;
            }
        }
    }
    (has_ins, has_del)
}

/// Zero-width sibling markers a paragraph-mark join may step over
/// (bookmarks, comment/permission/move/customXml range delimiters, proof
/// errors). These occupy no space in the flow, so removing a paragraph
/// break joins ACROSS them — but never across content (a table, an sdt, …).
/// The element kinds are the shared `resolution_rules::ZERO_WIDTH_BODY_MARKER_NAMES`
/// enumeration; the model path's `is_zero_width_marker_block` (tracked_model.rs)
/// consults the same list over its OpaqueBlocks, so the two cannot diverge on
/// which joins happen. Extraction stays byte-local: `is_w_tag` matches each
/// name in the `w:` namespace over the raw XML sibling.
fn is_zero_width_body_marker(element: &Element) -> bool {
    crate::resolution_rules::ZERO_WIDTH_BODY_MARKER_NAMES
        .iter()
        .any(|tag| is_w_tag(element, tag))
}

/// A table this resolution empties COMPLETELY: it has rows and every one
/// carries the drop-class row marker (`w:trPr/w:del` on accept,
/// `w:trPr/w:ins` on reject — a STACKED row carries both and so drops in
/// both full resolutions). The revision pass then removes the rowless shell
/// (§17.4.37), so a paragraph-mark join must treat the table as absent.
/// Byte-level counterpart of the model path's `table_emptied_by_accept_reject`
/// (tracked_model.rs): both extract each row's `(has_ins, has_del)` from their
/// own representation (here the `w:trPr/w:ins` / `w:trPr/w:del` markers) and
/// hand them to the shared `resolution_rules::table_emptied_by_resolution`, so
/// the "had rows and none survive" composition and its per-row survival cannot
/// diverge on which joins step over a vanishing table.
fn table_emptied_by_resolution(tbl: &Element, keep_inserted: bool) -> bool {
    let rows = tbl.children.iter().filter_map(|c| match c {
        XMLNode::Element(el) if is_w_tag(el, "tr") => Some(row_survives(el, keep_inserted)),
        _ => None,
    });
    crate::resolution_rules::table_emptied_by_resolution(rows)
}

/// Whether a `w:tr` survives this full resolution
/// (`resolution_rules::row_survives_resolution`): its own `w:trPr` marks and
/// at least one of its cells' `w:tcPr` marks survive. Cells inside a row-level
/// content control or custom XML wrapper count as the row's cells.
fn row_survives(tr: &Element, keep_inserted: bool) -> bool {
    crate::resolution_rules::row_survives_resolution(
        (has_row_tracking(tr, "ins"), has_row_tracking(tr, "del")),
        row_cells(tr).iter().map(|tc| {
            (
                has_cell_tracking(tc, "cellIns"),
                has_cell_tracking(tc, "cellDel"),
            )
        }),
        keep_inserted,
    )
}

/// A `w:tr`'s cells, including those inside a row-level content control or
/// custom XML wrapper.
fn row_cells(tr: &Element) -> Vec<&Element> {
    fn collect<'a>(el: &'a Element, out: &mut Vec<&'a Element>) {
        for child in &el.children {
            if let XMLNode::Element(child) = child {
                if is_w_tag(child, "tc") {
                    out.push(child);
                } else if is_w_tag(child, "sdt")
                    || is_w_tag(child, "sdtContent")
                    || is_w_tag(child, "customXml")
                {
                    collect(child, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    collect(tr, &mut out);
    out
}

/// Revisions resolved by dropping a whole row: its own row mark, or else each
/// of its (all removed) cells' marks, plus everything inside.
fn dropped_row_revision_count(tr: &Element, row_mark: &str) -> u32 {
    let own = if has_row_tracking(tr, row_mark) {
        1
    } else {
        row_cells(tr).len() as u32
    };
    own + count_nested_revisions(tr)
}

/// Whether this resolution EMPTIES the paragraph: it HAS content children and
/// every one is drop-class for this resolution (`w:ins`/`w:moveTo` wrappers on
/// reject, `w:del`/`w:moveFrom` on accept; move-range delimiters go with the
/// move either way). Zero-width markers and `pPr` are not content. Structural
/// carriers — `w:sectPr` or `w:numPr` inside `pPr` — make the paragraph
/// non-droppable regardless. Byte-level counterpart of the model path's
/// `paragraph_emptied_by_accept_reject` (tracked_model.rs): a paragraph that
/// was ALREADY empty in the base (mark merely became inserted to append a new
/// paragraph after it) is base content and must survive, hence the
/// had-content requirement.
fn paragraph_emptied_by_resolution(p: &Element, keep_inserted: bool) -> bool {
    if !is_w_tag(p, "p") {
        return false;
    }
    let mut had_content = false;
    for c in &p.children {
        let XMLNode::Element(el) = c else { continue };
        if is_w_tag(el, "pPr") {
            let structural = el.children.iter().any(|pc| {
                matches!(pc, XMLNode::Element(e)
                    if is_w_tag(e, "sectPr") || is_w_tag(e, "numPr"))
            });
            if structural {
                return false;
            }
            continue;
        }
        if is_zero_width_body_marker(el) || is_move_range_marker(el) || is_comment_reference_run(el)
        {
            continue;
        }
        // Classify this content child and consult the shared survival rule
        // (`resolution_rules::tracked_class_survives`, the same rule the model
        // path's segment retain uses): an `w:ins`/`w:moveTo` wrapper is
        // insertion-class, `w:del`/`w:moveFrom` deletion-class, a bare content
        // run neither (always survives). If anything survives, not emptied.
        let has_ins = is_w_tag(el, "ins") || is_w_tag(el, "moveTo");
        let has_del = is_w_tag(el, "del") || is_w_tag(el, "moveFrom");
        if crate::resolution_rules::tracked_class_survives(has_ins, has_del, keep_inserted) {
            return false;
        }
        had_content = true;
    }
    had_content
}

/// A run that holds only a comment reference: a zero-width anchor, not content.
fn is_comment_reference_run(el: &Element) -> bool {
    is_w_tag(el, "r")
        && el
            .children
            .iter()
            .any(|c| matches!(c, XMLNode::Element(child) if is_w_tag(child, "commentReference")))
        && el.children.iter().all(|c| match c {
            XMLNode::Element(child) => {
                is_w_tag(child, "rPr") || is_w_tag(child, "commentReference")
            }
            _ => true,
        })
}

/// The zero-width anchors (range markers, comment-reference runs) that
/// outlast removing the emptied paragraph `donor`: its own, and the range
/// markers its dropped carriers hold unless a pair lies wholly within one
/// removed stretch. The byte twin of `tracked_model::anchors_outlasting_paragraph`.
fn anchors_outlasting_paragraph(donor: Element, keep_inserted: bool) -> Vec<XMLNode> {
    let mut anchors = Vec::new();
    let mut stretch = Vec::new();
    for child in donor.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        let has_ins = is_w_tag(&el, "ins") || is_w_tag(&el, "moveTo");
        let has_del = is_w_tag(&el, "del") || is_w_tag(&el, "moveFrom");
        if (has_ins || has_del)
            && !crate::resolution_rules::tracked_class_survives(has_ins, has_del, keep_inserted)
        {
            collect_tree_range_markers(&el, &mut stretch);
        } else if tree_range_marker_of(&el).is_some() || is_comment_reference_run(&el) {
            keep_removed_stretch_markers(&mut stretch, &mut anchors);
            anchors.push(XMLNode::Element(el));
        }
    }
    keep_removed_stretch_markers(&mut stretch, &mut anchors);
    anchors
}

/// Keep the anchors of an emptied paragraph a blocked join dropped from just
/// before `parent.children[at]`: at the end of the nearest paragraph before
/// it, or failing that at the start of the nearest one after. The byte twin of
/// `tracked_model::hold_anchors_beside_dropped_paragraph`.
fn hold_anchors_beside_dropped_paragraph(parent: &mut Element, at: usize, anchors: Vec<XMLNode>) {
    if anchors.is_empty() {
        return;
    }
    let is_paragraph = |c: &XMLNode| matches!(c, XMLNode::Element(el) if is_w_tag(el, "p"));
    if let Some(before) = parent.children[..at].iter().rposition(is_paragraph) {
        let XMLNode::Element(holder) = &mut parent.children[before] else {
            unreachable!("the holder is a paragraph")
        };
        holder.children.extend(anchors);
    } else if let Some(after) = parent.children[at..].iter().position(is_paragraph) {
        let XMLNode::Element(holder) = &mut parent.children[at + after] else {
            unreachable!("the holder is a paragraph")
        };
        let insert_at = holder
            .children
            .iter()
            .position(|c| matches!(c, XMLNode::Element(el) if is_w_tag(el, "pPr")))
            .map_or(0, |index| index + 1);
        holder.children.splice(insert_at..insert_at, anchors);
    }
}

/// Join paragraphs whose mark this resolution removes (ECMA-376 §17.13.5.15 /
/// §17.13.5.20): accepting a mark DELETION — or rejecting a mark INSERTION —
/// removes the paragraph break, so the paragraph's content merges into the
/// FOLLOWING paragraph (whose properties win). A STACKED mark (both markers:
/// inserted by one pending revision, deleted by another) joins in BOTH full
/// resolutions — the four origin rules, same as inline text and rows.
///
/// The join steps over zero-width sibling markers (bookmarkEnd etc. — see
/// `is_zero_width_body_marker`) but stops at any CONTENT sibling: removing
/// a paragraph break never teleports text across a table.
///
/// This is the byte-level counterpart of the model path's
/// `merge_marked_paragraphs_bare` (tracked_model.rs); it must run BEFORE the
/// revision pass, which would otherwise silently drop the markers and leave
/// the paragraphs split — diverging from both the IR path and real Word.
fn join_mark_resolved_paragraphs(
    parent: &mut Element,
    keep_inserted: bool,
    stats: &mut NormalizeStats,
) {
    let mut i = 0;
    while i < parent.children.len() {
        let joins = match &parent.children[i] {
            XMLNode::Element(el) if is_w_tag(el, "p") => {
                let (has_ins, has_del) = para_mark_markers(el);
                crate::resolution_rules::para_mark_join_needed(has_ins, has_del, keep_inserted)
            }
            _ => false,
        };
        if !joins {
            i += 1;
            continue;
        }
        // Find the join target: the next paragraph sibling, stepping over
        // zero-width markers, non-element nodes, and tables this resolution
        // empties of every row (they vanish on the same pass — Word rejoins
        // one logical paragraph split around all-tracked tables; mirrors the
        // model path's `table_emptied_by_accept_reject` step-over), but
        // stopping at any other element — surviving content blocks the join.
        let mut next_p = None;
        for (offset, c) in parent.children[(i + 1)..].iter().enumerate() {
            match c {
                XMLNode::Element(el) if is_w_tag(el, "p") => {
                    next_p = Some(offset);
                    break;
                }
                XMLNode::Element(el) if is_zero_width_body_marker(el) => continue,
                XMLNode::Element(el)
                    if is_w_tag(el, "tbl") && table_emptied_by_resolution(el, keep_inserted) =>
                {
                    continue;
                }
                XMLNode::Element(_) => break,
                _ => continue,
            }
        }
        let Some(offset) = next_p else {
            // No join target (a surviving table follows, or nothing does).
            // Drop a donor this resolution leaves EMPTY: a fully-inserted
            // paragraph rejected — or a fully-deleted paragraph accepted — has
            // no content to carry and its mark is resolved away, so Word
            // removes it rather than leaving an empty husk (wild-witnessed:
            // an inserted paragraph directly before a retained table must
            // vanish on reject). A donor with surviving content stays, its
            // mark becoming an ordinary terminating mark. Mirrors the model
            // path's `paragraph_emptied_by_accept_reject` arm, including its
            // guards: structural carriers (`w:sectPr` / `w:numPr` in pPr)
            // never drop, an already-empty base paragraph never drops, and
            // the LAST block of a container never drops (a body/cell must
            // still end with a paragraph).
            let container_may_be_empty = is_w_tag(parent, "sdtContent");
            let drop_empty = match &parent.children[i] {
                XMLNode::Element(el) => {
                    paragraph_emptied_by_resolution(el, keep_inserted)
                        && (container_may_be_empty
                            || parent.children[(i + 1)..].iter().any(|c| {
                                matches!(c, XMLNode::Element(el)
                                    if is_w_tag(el, "p") || is_w_tag(el, "tbl"))
                            }))
                }
                _ => false,
            };
            if drop_empty {
                let XMLNode::Element(donor) = parent.children.remove(i) else {
                    unreachable!("drop_empty matched an element")
                };
                hold_anchors_beside_dropped_paragraph(
                    parent,
                    i,
                    anchors_outlasting_paragraph(donor, keep_inserted),
                );
                stats.revisions_resolved += 1;
            } else {
                i += 1;
            }
            continue;
        };
        let j = i + 1 + offset;
        let XMLNode::Element(donor) = parent.children.remove(i) else {
            unreachable!()
        };
        // Donor content = everything except its pPr (target's properties win).
        let donor_content: Vec<XMLNode> = donor
            .children
            .into_iter()
            .filter(|c| !matches!(c, XMLNode::Element(el) if is_w_tag(el, "pPr")))
            .collect();
        stats.revisions_resolved += 1;
        let XMLNode::Element(target) = &mut parent.children[j - 1] else {
            unreachable!()
        };
        // Insert donor content right after the target's pPr (or at the front).
        let insert_at = target
            .children
            .iter()
            .position(|c| matches!(c, XMLNode::Element(el) if is_w_tag(el, "pPr")))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        for (k, node) in donor_content.into_iter().enumerate() {
            target.children.insert(insert_at + k, node);
        }
        // Do not advance: the new occupant of slot i may itself join.
    }
}

fn normalize_children(parent: &mut Element, stats: &mut NormalizeStats, inside_opaque: bool) {
    settle_inline_sdt_deletion_envelopes(parent, true, stats);
    // Join paragraphs whose mark deletion this accept applies (must precede
    // the revision pass, which drops the markers).
    join_mark_resolved_paragraphs(parent, /*keep_inserted=*/ true, stats);

    // First pass: remove *PrChange children from property elements.
    // We do this before the main pass because these are children of property
    // elements (w:rPr, w:pPr, etc.), not direct content flow elements.
    remove_pr_changes(parent, stats, inside_opaque);

    // Second pass: handle content revisions (ins/del/moveFrom/moveTo/delText).
    // We need to rebuild the children list because unwrapping can expand one
    // child into multiple children.
    let mut new_children: Vec<XMLNode> = Vec::with_capacity(parent.children.len());
    // Where a carrier was dropped or unwrapped: the runs that meet there may
    // be the pieces of one run an edit split.
    let mut seams: Vec<usize> = Vec::new();

    // Range markers the dropped carriers hold, kept where they were.
    let mut removed_stretch: Vec<Element> = Vec::new();

    for child in parent.children.drain(..) {
        match &child {
            XMLNode::Element(el) if is_drop_revision(el) => {
                collect_tree_range_markers(el, &mut removed_stretch);
            }
            XMLNode::Element(_) => {
                keep_removed_stretch_markers(&mut removed_stretch, &mut new_children)
            }
            _ => {}
        }
        match child {
            XMLNode::Element(el) if is_drop_revision(&el) => {
                // w:del, w:moveFrom → drop entirely
                seams.push(new_children.len());
                let count = 1 + count_nested_revisions(&el);
                if inside_opaque {
                    stats.opaque_resolved += count;
                } else {
                    stats.revisions_resolved += count;
                }
            }
            XMLNode::Element(el) if is_del_text(&el) => {
                // w:delText → drop
                if inside_opaque {
                    stats.opaque_resolved += 1;
                } else {
                    stats.revisions_resolved += 1;
                }
            }
            XMLNode::Element(mut el) if is_unwrap_revision(&el) => {
                // w:ins, w:moveTo → unwrap: keep children, drop the wrapper
                if inside_opaque {
                    stats.opaque_resolved += 1;
                } else {
                    stats.revisions_resolved += 1;
                }
                // Recursively normalize children before promoting them
                normalize_children(&mut el, stats, inside_opaque);
                // Promote all children of the revision wrapper
                seams.push(new_children.len());
                new_children.extend(el.children);
                seams.push(new_children.len());
            }
            XMLNode::Element(el) if is_move_range_marker(&el) => {
                // Move range delimiters go with the move they bracket
                // (accept resolved the move: moveTo content kept, moveFrom
                // dropped — see is_move_range_marker for why these must not
                // survive).
                if inside_opaque {
                    stats.opaque_resolved += 1;
                } else {
                    stats.revisions_resolved += 1;
                }
            }
            XMLNode::Element(el) if is_w_tag(&el, "tr") && !row_survives(&el, true) => {
                // Row-level tracked deletion (w:trPr/w:del): drop the entire
                // row on accept. ECMA-376 §17.13.5.12 — a w:del inside w:trPr
                // marks the whole row as deleted; accepting a deletion yields
                // the document as if the content were never present, so the
                // row (and all its cell content) is removed. This mirrors the
                // reject path for inserted rows (w:trPr/w:ins) above, and keeps
                // this path in agreement with the IR accept path
                // (tracked_model.rs), which also removes the row. A row whose
                // every cell is a w:cellDel goes too: CT_Row needs a cell
                // (`resolution_rules::row_survives_resolution`).
                let count = dropped_row_revision_count(&el, "del");
                if inside_opaque {
                    stats.opaque_resolved += count;
                } else {
                    stats.revisions_resolved += count;
                }
            }
            XMLNode::Element(el) if is_w_tag(&el, "tc") && has_cell_tracking(&el, "cellDel") => {
                // Cell-level tracked deletion (w:tcPr/w:cellDel, §17.13.5.1):
                // drop the entire cell on accept — same semantics as the IR
                // accept path (tracked_model.rs), which removes the cell. A
                // STACKED cell (cellIns + cellDel) also lands here: it drops
                // in both full resolutions (origin rules).
                let count = 1 + count_nested_revisions(&el);
                if inside_opaque {
                    stats.opaque_resolved += count;
                } else {
                    stats.revisions_resolved += count;
                }
            }
            XMLNode::Element(mut el) => {
                // Non-revision element: recurse into it
                let had_nested_revision = has_nested_content_revision(&el);
                let child_opaque = inside_opaque || is_opaque_container(&el);
                let cell_merge = if is_w_tag(&el, "tc") {
                    take_cell_merge(&mut el, true)
                } else {
                    None
                };
                normalize_children(&mut el, stats, child_opaque);
                // A surviving cell's w:cellIns marker (§17.13.5.2) is resolved
                // on accept: the cell stays, the marker goes.
                if is_w_tag(&el, "tc") {
                    let mut stripped = strip_cell_tracking_markers(&mut el);
                    if let Some(merge) = cell_merge {
                        apply_cell_merge(&mut el, merge);
                        stripped += 1;
                    }
                    if inside_opaque {
                        stats.opaque_resolved += stripped;
                    } else {
                        stats.revisions_resolved += stripped;
                    }
                }
                if is_w_tag(&el, "tbl") {
                    settle_resolved_cell_merges(&mut el);
                }
                // OOXML §17.4.37: a table must contain a non-zero number of rows.
                // After acceptance drops all deleted rows, a table can end up with
                // zero w:tr children. Remove it rather than producing invalid XML.
                if is_w_tag(&el, "tbl") && !has_any_row(&el) {
                    // Table is now empty — drop it entirely.
                } else if is_revision_emptied_hyperlink(&el, had_nested_revision) {
                    // See the reject path above: inner-run tracking is the legal
                    // OOXML carrier for a paragraph-level hyperlink insertion or
                    // deletion, so an emptied wrapper is part of that revision.
                } else {
                    new_children.push(XMLNode::Element(el));
                }
            }
            other => {
                // Text, Comment, CData, PI nodes — keep as-is
                new_children.push(other);
            }
        }
    }
    keep_removed_stretch_markers(&mut removed_stretch, &mut new_children);

    if inside_opaque {
        rejoin_runs_at_seams(&mut new_children, &seams);
    }
    parent.children = new_children;
}

/// Rejoin the plain text runs that meet at a resolved carrier's seam. A
/// tracked edit inside a run splits it around the change; once the change is
/// resolved the pieces sit side by side with the same properties, and the run
/// they came from is the one spelling of that text. Only runs meeting at a
/// seam are joined, so runs the source itself kept apart stay apart. The
/// joined text is marked `xml:space="preserve"` exactly when it has leading
/// or trailing XML whitespace, as Word writes it.
pub(crate) fn rejoin_runs_at_seams(children: &mut Vec<XMLNode>, seams: &[usize]) {
    let mut seams: Vec<usize> = seams.to_vec();
    seams.sort_unstable();
    seams.dedup();
    for &seam in seams.iter().rev() {
        if seam == 0 || seam >= children.len() {
            continue;
        }
        let (XMLNode::Element(left), XMLNode::Element(right)) =
            (&children[seam - 1], &children[seam])
        else {
            continue;
        };
        let (Some(left_text), Some(right_text)) = (plain_run_text(left), plain_run_text(right))
        else {
            continue;
        };
        if left.attributes != right.attributes || run_properties(left) != run_properties(right) {
            continue;
        }
        let text = format!("{left_text}{right_text}");
        children.remove(seam);
        let XMLNode::Element(left) = &mut children[seam - 1] else {
            unreachable!("checked above");
        };
        let t = left
            .children
            .iter_mut()
            .find_map(|child| match child {
                XMLNode::Element(el) if is_w_tag(el, "t") => Some(el),
                _ => None,
            })
            .expect("a plain run holds a w:t");
        t.children = vec![XMLNode::Text(text.clone())];
        if !crate::word_xml::needs_space_preserve(&text) {
            t.attributes.retain(|name, _| {
                !(name.local_name == "space" && name.prefix.as_deref() == Some("xml"))
            });
        } else {
            crate::xml_attrs::attr_set(t, "xml:space", "preserve");
        }
    }
}

/// The text of a `w:r` holding only optional `w:rPr` and one `w:t`.
fn plain_run_text(run: &Element) -> Option<String> {
    if !is_w_tag(run, "r") {
        return None;
    }
    let mut text = None;
    for child in &run.children {
        match child {
            XMLNode::Element(el) if is_w_tag(el, "rPr") => {}
            XMLNode::Element(el) if is_w_tag(el, "t") && text.is_none() => {
                let mut value = String::new();
                for node in &el.children {
                    match node {
                        XMLNode::Text(t) => value.push_str(t),
                        _ => return None,
                    }
                }
                text = Some(value);
            }
            _ => return None,
        }
    }
    text
}

/// A run's `w:rPr`, where an empty one is the same as none.
fn run_properties(run: &Element) -> Option<&Element> {
    run.children.iter().find_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, "rPr") && !el.children.is_empty() => Some(el),
        _ => None,
    })
}

fn is_revision_emptied_hyperlink(element: &Element, had_nested_revision: bool) -> bool {
    had_nested_revision && is_w_tag(element, "hyperlink") && !has_hyperlink_payload(element)
}

fn has_nested_content_revision(element: &Element) -> bool {
    element.children.iter().any(|child| {
        matches!(child, XMLNode::Element(child) if is_content_revision(child)
            || is_del_text(child)
            || has_nested_content_revision(child))
    })
}

fn has_hyperlink_payload(element: &Element) -> bool {
    element.children.iter().any(|child| {
        let XMLNode::Element(child) = child else {
            return false;
        };
        if matches!(child.name.as_str(), "t" | "instrText" | "delText")
            && child
                .children
                .iter()
                .any(|node| matches!(node, XMLNode::Text(text) if !text.is_empty()))
        {
            return true;
        }
        if matches!(
            child.name.as_str(),
            "tab"
                | "br"
                | "cr"
                | "drawing"
                | "object"
                | "pict"
                | "sym"
                | "noBreakHyphen"
                | "softHyphen"
                | "fldChar"
                | "footnoteReference"
                | "endnoteReference"
                | "commentReference"
        ) {
            return true;
        }
        has_hyperlink_payload(child)
    })
}

/// Current-property children that are NOT part of a `*PrChange`'s
/// previous-properties payload and therefore must survive a reject restore:
///
/// - `pPr`: the paragraph-mark `w:rPr` and `w:sectPr` — `pPrChange`'s child
///   pPr is CT_PPrBase (§17.13.5.29), which carries neither;
/// - mark `rPr`: the mark-revision markers `w:ins`/`w:del`/`w:moveFrom`/
///   `w:moveTo` (CT_ParaRPr) — a separate revision axis from the mark's
///   rPrChange, and the paragraph-join machinery still needs them;
/// - `trPr` / `tcPr`: the row/cell revision markers — `trPrChange`/
///   `tcPrChange` carry the *PrBase forms, and the content pass resolves
///   the markers itself;
/// - `sectPr`: header/footer references — `sectPrChange`'s child sectPr is
///   CT_SectPrBase (§17.13.5.32), which excludes them.
fn reject_restore_keeps_current_child(parent_local: &str, child: &Element) -> bool {
    let child_local = local_element_name(child);
    match parent_local {
        "pPr" => matches!(child_local, "rPr" | "sectPr"),
        "rPr" => matches!(child_local, "ins" | "del" | "moveFrom" | "moveTo"),
        "trPr" => matches!(child_local, "ins" | "del"),
        "tcPr" => matches!(child_local, "cellIns" | "cellDel" | "cellMerge"),
        "sectPr" => matches!(child_local, "headerReference" | "footerReference"),
        _ => false,
    }
}

/// Reject a property element's `*PrChange` record in place: RESTORE the
/// previous properties it carries (§17.13.5.29–.32 — the record's child
/// property element is the complete previous state). Children listed by
/// [`reject_restore_keeps_current_child`] are kept from the CURRENT element,
/// placed per their position in the parent's content model (markers lead
/// CT_ParaRPr and CT_SectPr, trail the others).
///
/// A record with no property child carries no previous state: it is removed
/// without touching the current properties, mirroring the model path
/// (word_ir's extract_*_change ignores such records).
fn restore_pr_change_in_place(el: &mut Element, stats: &mut NormalizeStats, inside_opaque: bool) {
    let Some(pos) = el
        .children
        .iter()
        .position(|c| matches!(c, XMLNode::Element(e) if is_pr_change(e)))
    else {
        return;
    };
    let XMLNode::Element(record) = el.children.remove(pos) else {
        unreachable!("position matched an element");
    };
    if inside_opaque {
        stats.opaque_resolved += 1;
    } else {
        stats.revisions_resolved += 1;
    }
    // Drop (and count) any additional records — invalid markup, but never
    // restore from a second record after the first already won.
    el.children.retain(|c| match c {
        XMLNode::Element(e) if is_pr_change(e) => {
            if inside_opaque {
                stats.opaque_resolved += 1;
            } else {
                stats.revisions_resolved += 1;
            }
            false
        }
        _ => true,
    });

    let Some(previous) = record.children.into_iter().find_map(|c| match c {
        XMLNode::Element(e) => Some(e),
        _ => None,
    }) else {
        return;
    };

    let parent_local = local_element_name(el).to_string();
    let (kept, current_properties): (Vec<XMLNode>, Vec<XMLNode>) = el.children.drain(..).partition(|c| {
        matches!(c, XMLNode::Element(e) if reject_restore_keeps_current_child(&parent_local, e))
    });
    let mut restored = previous.children;
    if parent_local == "tcPr" {
        // A `w:cellMerge` in the prior tcPr (Word's Compare writes one there)
        // marks the change, it is no property to restore; the cell's own one
        // is kept above and resolved with the cell (`take_cell_merge`).
        restored.retain(|c| !matches!(c, XMLNode::Element(e) if is_w_tag(e, "cellMerge")));
    }
    el.children = match parent_local.as_str() {
        // Word resolves a rejected section change RELATIVE to the current
        // section: the previous payload's children replace their current
        // counterparts and omitted children keep their current values
        // (observed in Word: rejecting a sectPrChange whose previous sectPr
        // holds only pgSz keeps the current pgMar, cols and docGrid). The
        // typed model resolves the same way
        // (`apply_previous_section_properties_relative`).
        "sectPr" => kept
            .into_iter()
            .chain(overlay_section_properties(current_properties, restored))
            .collect(),
        "rPr" => kept.into_iter().chain(restored).collect(),
        _ => restored.into_iter().chain(kept).collect(),
    };
}

/// Overlay a previous `sectPr` payload onto the current section properties, in
/// CT_SectPr order. Children of unknown elements stay after the known ones.
fn overlay_section_properties(current: Vec<XMLNode>, previous: Vec<XMLNode>) -> Vec<XMLNode> {
    use crate::docx_validate_ordering::SECTPR_PROPERTY_ORDER;
    let named = |nodes: Vec<XMLNode>| -> Vec<(String, XMLNode)> {
        nodes
            .into_iter()
            .filter_map(|node| match &node {
                XMLNode::Element(element) => Some((local_element_name(element).to_string(), node)),
                _ => None,
            })
            .collect()
    };
    let mut current = named(current);
    let mut previous = named(previous);
    let take = |list: &mut Vec<(String, XMLNode)>, name: &str| {
        list.iter()
            .position(|(child, _)| child == name)
            .map(|index| list.remove(index).1)
    };
    let mut merged = Vec::new();
    for name in SECTPR_PROPERTY_ORDER {
        let from_previous = take(&mut previous, name);
        let from_current = take(&mut current, name);
        if let Some(node) = from_previous.or(from_current) {
            merged.push(node);
        }
    }
    merged.extend(current.into_iter().map(|(_, node)| node));
    merged.extend(previous.into_iter().map(|(_, node)| node));
    merged
}

/// Reject *PrChange records in all property elements, recursively: each
/// record's previous properties are restored (see
/// [`restore_pr_change_in_place`]); the accept-side counterpart is
/// [`remove_pr_changes`], which keeps the new properties and drops the
/// record.
fn reject_pr_changes(element: &mut Element, stats: &mut NormalizeStats, inside_opaque: bool) {
    for child in element.children.iter_mut() {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        let child_opaque = inside_opaque || is_opaque_container(el);
        restore_pr_change_in_place(el, stats, child_opaque);
        reject_pr_changes(el, stats, child_opaque);
    }
}

/// Remove *PrChange elements from all property elements, recursively.
///
/// Walks the tree and for each property element (w:rPr, w:pPr, etc.),
/// removes any *PrChange child while keeping the parent property element.
fn remove_pr_changes(element: &mut Element, stats: &mut NormalizeStats, inside_opaque: bool) {
    for child in element.children.iter_mut() {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        // Compute opaque flag before the closure to avoid borrow conflict
        let child_opaque = inside_opaque || is_opaque_container(el);

        // Remove *PrChange children from this element
        el.children.retain(|c| match c {
            XMLNode::Element(e) if is_pr_change(e) => {
                if child_opaque {
                    stats.opaque_resolved += 1;
                } else {
                    stats.revisions_resolved += 1;
                }
                false
            }
            _ => true,
        });

        // Recurse into all remaining children
        remove_pr_changes(el, stats, child_opaque);
    }
}

/// Resolve a `*PrChange` record carried DIRECTLY by a single property element
/// `el` (e.g. a body-level `w:sectPr` with a `w:sectPrChange` child), reusing
/// the exact byte-path accept/reject logic so both reject/accept paths agree.
///
/// - `keep_new = true` (accept): drop the record, keep the current properties
///   (mirrors [`remove_pr_changes`], but applied to `el`'s OWN children rather
///   than its grandchildren).
/// - `keep_new = false` (reject): restore the complete previous properties the
///   record carries and drop it (delegates to [`restore_pr_change_in_place`],
///   §17.13.5.29–.32).
///
/// The transform is purely structural on the raw element tree, so unmodeled
/// properties (cols, docGrid, header/footer references, …) survive untouched —
/// it is the same losslessness the byte-path relies on. Used by the model
/// projection path to resolve an imported `sectPrChange` in the verbatim
/// `sectPr` cache it feeds to serialization, keeping it consistent with
/// `reject_all_docx` / `normalize_docx`.
pub(crate) fn resolve_pr_change_on_element(el: &mut Element, keep_new: bool) {
    let mut stats = NormalizeStats::default();
    if keep_new {
        // Accept: drop the record directly from `el`'s children, keep the rest.
        el.children
            .retain(|c| !matches!(c, XMLNode::Element(e) if is_pr_change(e)));
    } else {
        // Reject: restore the previous properties the record carries.
        restore_pr_change_in_place(el, &mut stats, /*inside_opaque=*/ false);
    }
}

/// Count revision elements nested inside a dropped element (w:del, w:moveFrom).
/// These are counted because they are also being resolved (removed) as part of
/// dropping the parent.
fn count_nested_revisions(element: &Element) -> u32 {
    let mut count = 0u32;
    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if is_content_revision(el) || is_pr_change(el) || is_del_text(el) {
            count += 1;
        }
        count += count_nested_revisions(el);
    }
    count
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::{DocxArchive, DocxFile};

    /// The two forms of the ONE carrier inventory must stay in lockstep: every
    /// element-name entry has both a ` `-suffixed and `>`-suffixed byte marker,
    /// and the only byte marker without an element-name entry is `delText`
    /// (deleted-text content, deliberately excluded — see
    /// `REVISION_ELEMENT_LOCAL_NAMES`). A tag added to one list but not the
    /// other trips this immediately, so the opaque-interior tree walk and the
    /// byte gate can never disagree on what counts as a revision.
    #[test]
    fn opaque_interior_inventory_mirrors_byte_markers() {
        use std::collections::HashSet;
        // Local names implied by the byte markers: strip "<w:" and the trailing
        // " " / ">" (or nothing, for the prefix-only "<w:delText"/"<w:rPrChange"
        // forms that have no bracket in the marker).
        let byte_local_names: HashSet<String> = REVISION_BYTE_MARKERS
            .iter()
            .map(|m| {
                let s = std::str::from_utf8(m).unwrap();
                let s = s.strip_prefix("<w:").unwrap();
                s.trim_end_matches([' ', '>']).to_string()
            })
            .collect();

        let mut expected: HashSet<String> = REVISION_ELEMENT_LOCAL_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect();
        // `delText` is the single content-not-carrier tag present only in the
        // byte gate.
        expected.insert("delText".to_string());

        assert_eq!(
            byte_local_names, expected,
            "REVISION_BYTE_MARKERS and REVISION_ELEMENT_LOCAL_NAMES describe the \
             same carrier inventory (modulo delText); they have drifted"
        );
    }

    /// Reference (tree-based) per-part scan, kept ONLY in tests as the oracle the
    /// streaming `scan_part_streaming` must reproduce byte-for-byte. This is the
    /// pre-Rung-6 implementation: build the whole `xmltree` tree, then walk it
    /// with `is_w_tag` / `is_pr_change`.
    fn reference_scan_part(bytes: &[u8]) -> (PartRevisionCounts, PartCommentCounts) {
        fn walk(element: &Element, rev: &mut PartRevisionCounts, com: &mut PartCommentCounts) {
            if is_w_tag(element, "ins") {
                rev.ins += 1;
            } else if is_w_tag(element, "del") {
                rev.del += 1;
            } else if is_w_tag(element, "moveFrom") {
                rev.move_from += 1;
            } else if is_w_tag(element, "moveTo") {
                rev.move_to += 1;
            } else if is_w_tag(element, "delText") {
                rev.del_text += 1;
            } else if is_pr_change(element) {
                rev.format_pr_change += 1;
            } else if is_w_tag(element, "commentRangeStart")
                || is_w_tag(element, "commentRangeEnd")
                || is_w_tag(element, "commentReference")
            {
                com.anchors += 1;
            }
            for child in &element.children {
                if let XMLNode::Element(el) = child {
                    walk(el, rev, com);
                }
            }
        }
        let root = parse_xml(bytes).expect("reference parse");
        let mut rev = PartRevisionCounts::default();
        let mut com = PartCommentCounts::default();
        walk(&root, &mut rev, &mut com);
        (rev, com)
    }

    /// Streaming scan must equal the tree-based reference for every story part of
    /// every corpus document. This is the parity gate for the Rung-6 rewrite.
    /// Set `STEMMA_CORPUS_ROOT` and run with `--ignored`.
    #[test]
    #[ignore = "corpus sweep; set STEMMA_CORPUS_ROOT — verifies streaming preflight == tree-based"]
    fn streaming_preflight_matches_tree_on_corpus() {
        let root = match std::env::var("STEMMA_CORPUS_ROOT") {
            Ok(r) => r,
            Err(_) => {
                eprintln!("STEMMA_CORPUS_ROOT not set; skipping");
                return;
            }
        };

        // Walk for *.docx under the corpus root.
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
        collect_docx(std::path::Path::new(&root), &mut docs);
        docs.sort();
        assert!(!docs.is_empty(), "no .docx found under {root}");

        // The corpus has ~100k docs; cap the sweep so it finishes in CI-ish time.
        // Override with STEMMA_PREFLIGHT_PARITY_LIMIT (0 = no cap).
        let limit = std::env::var("STEMMA_PREFLIGHT_PARITY_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(3000);
        if limit > 0 && docs.len() > limit {
            // Even stride sample for breadth across the sorted corpus.
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
            let Ok(story_paths) = collect_normalizable_part_paths(&archive) else {
                continue;
            };
            for part_path in &story_paths {
                let Some(xml) = archive.get(part_path) else {
                    continue;
                };
                // The streaming and tree paths must agree, OR both error the
                // same way (we only compare the success path here).
                let stream = match scan_part_streaming(xml) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let reference = reference_scan_part(xml);
                if stream != reference {
                    mismatches.push(format!(
                        "{}::{part_path}: stream={:?} reference={:?}",
                        path.display(),
                        stream,
                        reference
                    ));
                }
            }
            checked += 1;
        }

        assert!(
            mismatches.is_empty(),
            "{} mismatch(es) across {checked} docs:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
        eprintln!("streaming preflight parity OK across {checked} corpus docs");
    }

    /// Build a minimal DOCX archive with a document.xml part.
    fn archive_with_document_xml(xml: &str) -> DocxArchive {
        DocxArchive::from_parts(vec![DocxFile {
            name: "word/document.xml".to_string(),
            data: xml.as_bytes().to_vec(),
        }])
    }

    /// Build a DOCX archive with document.xml and a rels file.
    fn archive_with_parts(parts: Vec<(&str, &str)>) -> DocxArchive {
        DocxArchive::from_parts(
            parts
                .into_iter()
                .map(|(name, data)| DocxFile {
                    name: name.to_string(),
                    data: data.as_bytes().to_vec(),
                })
                .collect(),
        )
    }

    #[test]
    fn inline_sdt_deletion_envelope_resolves_both_terminal_structures() {
        let direct = format!(
            r#"<w:document xmlns:w="{WORD_NS}"><w:body><w:p><w:r><w:t>LEFT</w:t></w:r><w:customXmlDelRangeStart w:id="7" w:author="Stemma"/><w:sdt><w:sdtPr><w:tag w:val="control"/></w:sdtPr><w:sdtContent><w:del w:id="8" w:author="Stemma"><w:r><w:delText>CONTROL</w:delText></w:r></w:del></w:sdtContent></w:sdt><w:customXmlDelRangeEnd w:id="7"/><w:r><w:t>RIGHT</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
        );
        let word_normalized = format!(
            r#"<w:document xmlns:w="{WORD_NS}"><w:body><w:p><w:r><w:t>LEFT</w:t></w:r><w:customXmlDelRangeStart w:id="7" w:author="Stemma"/><w:sdt><w:sdtPr><w:tag w:val="control"/></w:sdtPr><w:sdtContent><w:customXmlDelRangeEnd w:id="7"/><w:del w:id="8" w:author="Stemma"><w:r><w:delText>CONTROL</w:delText></w:r></w:del><w:customXmlDelRangeStart w:id="9" w:author="Stemma"/></w:sdtContent></w:sdt><w:customXmlDelRangeEnd w:id="9"/><w:r><w:t>RIGHT</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
        );

        for (label, xml) in [("direct", direct), ("Word-normalized", word_normalized)] {
            let archive = archive_with_document_xml(&xml);
            let (accepted, accepted_stats) =
                normalize_docx(&archive).expect("accept inline SDT deletion");
            let accepted_xml =
                String::from_utf8(accepted.get("word/document.xml").unwrap().to_vec()).unwrap();
            assert!(!accepted_xml.contains("<w:sdt"), "{label}: {accepted_xml}");
            assert!(
                !accepted_xml.contains("customXmlDelRange"),
                "{label}: {accepted_xml}"
            );
            assert!(!accepted_xml.contains("CONTROL"), "{label}: {accepted_xml}");
            assert!(accepted_xml.contains("LEFT"));
            assert!(accepted_xml.contains("RIGHT"));
            assert!(accepted_stats.opaque_nodes_resolved_revisions_count >= 1);

            let (rejected, rejected_stats) =
                reject_all_docx(&archive).expect("reject inline SDT deletion");
            let rejected_xml =
                String::from_utf8(rejected.get("word/document.xml").unwrap().to_vec()).unwrap();
            assert!(rejected_xml.contains("<w:sdt"), "{label}: {rejected_xml}");
            assert!(rejected_xml.contains("<w:t>CONTROL</w:t>"));
            assert!(
                !rejected_xml.contains("customXmlDelRange"),
                "{label}: {rejected_xml}"
            );
            assert!(!rejected_xml.contains("<w:del"));
            assert!(rejected_stats.opaque_nodes_resolved_revisions_count >= 1);
        }
    }

    // -----------------------------------------------------------------------
    // Main-part resolution (OPC §9.3) during normalization
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_errors_on_dangling_office_document_target() {
        // A package WITH _rels/.rels whose officeDocument relationship resolves
        // to an ABSENT part must ERROR out of normalize — never silently default
        // to word/document.xml and normalize the wrong part. The only tolerated
        // fallback is a package with NO _rels/.rels at all; a present-but-broken
        // relationship is a genuine defect (OPC §9.3).
        let archive = archive_with_parts(vec![(
            "_rels/.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        )]);
        let err = normalize_docx(&archive)
            .expect_err("a dangling officeDocument target must error, not default");
        assert!(
            matches!(err, NormalizeError::Package(_)),
            "expected a main-part resolution error, got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Preflight scan tests
    // -----------------------------------------------------------------------

    #[test]
    fn preflight_counts_ins_and_del() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>hello</w:t></w:r>
      </w:ins>
      <w:del w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>world</w:delText></w:r>
      </w:del>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let report = preflight_scan(&archive).unwrap();
        assert_eq!(report.totals.revisions.ins, 1);
        assert_eq!(report.totals.revisions.del, 1);
        assert_eq!(report.totals.revisions.del_text, 1);
    }

    #[test]
    fn auxiliary_grid_history_is_cleared_without_inventing_a_revision() {
        // Native Word discards standalone grid history without reverting the
        // current grid. ECMA-376 17.13.5.33 carries no author/date: this is
        // auxiliary table geometry, not an independently selectable change.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="3000"/><w:tblGridChange w:id="1"><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid></w:tblGridChange></w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"#;
        let archive = archive_with_document_xml(xml);
        for accept in [true, false] {
            let FragmentResolution::Resolved(bytes) =
                resolve_opaque_fragment_revisions(xml.as_bytes(), accept)
            else {
                panic!("auxiliary-only opaque fragment must be cleaned");
            };
            let terminal = std::str::from_utf8(&bytes).unwrap();
            assert!(terminal.contains("3000"));
            assert!(!terminal.contains("4000"));
            assert!(!terminal.contains("tblGridChange"));
        }
        let alternate_prefix = xml.replace("w:", "x:").replace("xmlns:w", "xmlns:x");
        let alternate = normalize_if_needed(&archive_with_document_xml(&alternate_prefix)).unwrap();
        assert!(
            !std::str::from_utf8(alternate.get("word/document.xml").unwrap())
                .unwrap()
                .contains("tblGridChange")
        );
        assert_eq!(
            preflight_scan(&archive).unwrap().totals.revisions.total(),
            0
        );
        assert!(
            has_revision_markup_fast(&archive),
            "auxiliary history still needs terminal cleanup"
        );
        let normalized = normalize_if_needed(&archive).unwrap();
        assert!(
            !std::str::from_utf8(normalized.get("word/document.xml").unwrap())
                .unwrap()
                .contains("tblGridChange")
        );
        let (accepted, accept_stats) = normalize_docx(&archive).unwrap();
        let (rejected, reject_stats) = reject_all_docx(&archive).unwrap();
        for (resolved, stats, retained, removed) in [
            (accepted, accept_stats, "3000", "4000"),
            (rejected, reject_stats, "3000", "4000"),
        ] {
            let xml = std::str::from_utf8(resolved.get("word/document.xml").unwrap()).unwrap();
            assert!(xml.contains(retained));
            assert!(!xml.contains(removed));
            assert!(!xml.contains("tblGridChange"));
            assert_eq!(stats.revisions_resolved, 0);
            assert_eq!(stats.parts_normalized, vec!["word/document.xml"]);
            assert_eq!(
                preflight_scan(&resolved).unwrap().totals.revisions.total(),
                0
            );
        }
    }

    #[test]
    fn omitted_grid_width_is_valid_but_malformed_width_is_not() {
        // §17.4.16: absence means a zero last-saved width. An unrelated
        // resolution must preserve that original absence on the wire.
        let xml = br#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#;
        for accept in [true, false] {
            let mut table = parse_xml(xml).unwrap();
            let original = table.clone();
            resolve_simple_column_slots(&mut table, accept, "word/document.xml").unwrap();
            assert_eq!(table, original);
            let bad = std::str::from_utf8(xml)
                .unwrap()
                .replace("<w:gridCol/>", "<w:gridCol w:w=\"invalid\"/>");
            let mut table = parse_xml(bad.as_bytes()).unwrap();
            assert!(matches!(
                resolve_simple_column_slots(&mut table, accept, "word/document.xml"),
                Err(NormalizeError::InvalidTableHistory { .. })
            ));
        }
    }

    #[test]
    fn a_grid_width_reads_as_import_reads_it() {
        // Import reads a decimal or unit-bearing ST_TwipsMeasure; so must the
        // resolution, which leaves a table without history as it was.
        for width in ["3908.5", "1in"] {
            let xml = format!(
                r#"<w:tbl xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tblGrid><w:gridCol w:w="{width}"/></w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#
            );
            for accept in [true, false] {
                let mut table = parse_xml(xml.as_bytes()).unwrap();
                let original = table.clone();
                resolve_simple_column_slots(&mut table, accept, "word/document.xml").unwrap();
                assert_eq!(table, original, "{width}");
            }
        }
    }

    #[test]
    fn coupled_grid_history_resolves_with_cell_properties() {
        // The previous grid and previous cell widths form one coordinate
        // frame. Reject restores both; Accept keeps both current values.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="3000"/><w:tblGridChange w:id="1"><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid></w:tblGridChange></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/><w:tcPrChange w:id="2" w:author="Reviewer"><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr></w:tcPrChange></w:tcPr><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"#;
        let archive = archive_with_document_xml(xml);
        assert_eq!(
            preflight_scan(&archive).unwrap().totals.revisions.total(),
            1
        );
        for (resolved, stats, retained, removed) in [
            {
                let (doc, stats) = normalize_docx(&archive).unwrap();
                (doc, stats, "3000", "4000")
            },
            {
                let (doc, stats) = reject_all_docx(&archive).unwrap();
                (doc, stats, "4000", "3000")
            },
        ] {
            let xml = std::str::from_utf8(resolved.get("word/document.xml").unwrap()).unwrap();
            assert!(xml.contains(retained));
            assert!(!xml.contains(removed), "mixed coordinate frame: {xml}");
            assert!(!xml.contains("tblGridChange"));
            assert!(!xml.contains("tcPrChange"));
            assert_eq!(stats.revisions_resolved, 1);
        }
    }

    #[test]
    fn nested_table_properties_do_not_own_the_outer_grid_history() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="3000"/><w:tblGridChange w:id="1"><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid></w:tblGridChange></w:tblGrid><w:tr><w:tc><w:tbl><w:tblGrid><w:gridCol w:w="1000"/><w:tblGridChange w:id="2"><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid></w:tblGridChange></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="1000" w:type="dxa"/><w:tcPrChange w:id="3" w:author="Reviewer"><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr></w:tcPrChange></w:tcPr><w:p/></w:tc></w:tr></w:tbl><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"#;
        let (rejected, stats) = reject_all_docx(&archive_with_document_xml(xml)).unwrap();
        let xml = std::str::from_utf8(rejected.get("word/document.xml").unwrap()).unwrap();
        assert!(xml.contains("3000"), "outer current grid must remain");
        assert!(xml.contains("2000"), "inner previous grid must return");
        assert!(!xml.contains("4000"));
        assert!(!xml.contains("1000"));
        assert!(!xml.contains("tblGridChange"));
        assert_eq!(stats.revisions_resolved, 1);
    }

    #[test]
    fn malformed_grid_history_is_not_silently_normalized_away() {
        for history in [
            r#"<w:tblGridChange w:id="1"/>"#,
            r#"<w:tblGridChange><w:tblGrid/></w:tblGridChange>"#,
            r#"<w:tblGridChange w:id="1"><w:tblGrid><w:gridCol w:w="bad"/></w:tblGrid></w:tblGridChange>"#,
            r#"<w:tblGridChange w:id="1"><w:tblGrid><w:unknown/></w:tblGrid></w:tblGridChange>"#,
        ] {
            let xml = format!(
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="3000"/>{history}</w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"#
            );
            let archive = archive_with_document_xml(&xml);
            assert!(matches!(
                resolve_opaque_fragment_revisions(xml.as_bytes(), true),
                FragmentResolution::InvalidTableHistory(_)
            ));
            for result in [normalize_docx(&archive), reject_all_docx(&archive)] {
                let Err(NormalizeError::InvalidTableHistory { part, reason }) = result else {
                    panic!("malformed history must refuse: {history}");
                };
                assert_eq!(part, "word/document.xml");
                assert!(!reason.is_empty());
            }
        }
    }

    #[test]
    fn preflight_counts_pr_changes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr>
        <w:pPrChange w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
          <w:pPr><w:jc w:val="left"/></w:pPr>
        </w:pPrChange>
      </w:pPr>
      <w:r>
        <w:rPr>
          <w:rPrChange w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z">
            <w:rPr><w:b/></w:rPr>
          </w:rPrChange>
        </w:rPr>
        <w:t>text</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let report = preflight_scan(&archive).unwrap();
        assert_eq!(report.totals.revisions.format_pr_change, 2);
    }

    #[test]
    fn preflight_counts_comment_anchors() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:commentRangeStart w:id="0"/>
      <w:r><w:t>commented text</w:t></w:r>
      <w:commentRangeEnd w:id="0"/>
      <w:r>
        <w:rPr><w:rStyle w:val="CommentReference"/></w:rPr>
        <w:commentReference w:id="0"/>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let report = preflight_scan(&archive).unwrap();
        assert_eq!(report.totals.comments.anchors, 3);
        assert_eq!(report.totals.revisions.total(), 0);
    }

    // -----------------------------------------------------------------------
    // Normalization tests
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_unwraps_ins() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>inserted</w:t></w:r>
      </w:ins>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = normalize_docx(&archive).unwrap();
        assert_eq!(stats.revisions_resolved, 1);

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();
        // The w:ins wrapper should be gone
        assert!(!result_xml.contains("w:ins"));
        // But the content should remain
        assert!(result_xml.contains("inserted"));
    }

    #[test]
    fn normalize_drops_del() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>keep</w:t></w:r>
      <w:del w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>deleted</w:delText></w:r>
      </w:del>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = normalize_docx(&archive).unwrap();
        // w:del + w:delText inside it
        assert!(stats.revisions_resolved >= 1);

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();
        assert!(!result_xml.contains("w:del"));
        assert!(!result_xml.contains("deleted"));
        assert!(result_xml.contains("keep"));
    }

    #[test]
    fn normalize_removes_pr_change() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr>
        <w:jc w:val="center"/>
        <w:pPrChange w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
          <w:pPr><w:jc w:val="left"/></w:pPr>
        </w:pPrChange>
      </w:pPr>
      <w:r><w:t>text</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = normalize_docx(&archive).unwrap();
        assert_eq!(stats.revisions_resolved, 1);

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();
        // pPrChange should be gone
        assert!(!result_xml.contains("pPrChange"));
        // But jc=center (current value) should remain
        assert!(result_xml.contains("center"));
        // The parent pPr should still exist
        assert!(result_xml.contains("pPr"));
    }

    #[test]
    fn normalize_preserves_comment_anchors() {
        // Comment anchors in the body (commentRangeStart/End, commentReference)
        // are zero-width decorations, not revisions: normalization leaves them
        // untouched. (Revisions INSIDE the comments part are resolved — see the
        // comment-body resolution suite; this test carries no comments part.)
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:commentRangeStart w:id="0"/>
      <w:r><w:t>commented</w:t></w:r>
      <w:commentRangeEnd w:id="0"/>
      <w:r>
        <w:commentReference w:id="0"/>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = normalize_docx(&archive).unwrap();
        assert_eq!(stats.revisions_resolved, 0);

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();
        assert!(result_xml.contains("commentRangeStart"));
        assert!(result_xml.contains("commentRangeEnd"));
        assert!(result_xml.contains("commentReference"));
    }

    #[test]
    fn normalize_handles_move_from_to() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:moveFrom w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>moved from here</w:t></w:r>
      </w:moveFrom>
      <w:moveTo w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>moved to here</w:t></w:r>
      </w:moveTo>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = normalize_docx(&archive).unwrap();
        assert!(stats.revisions_resolved >= 2);

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();
        assert!(!result_xml.contains("moveFrom"));
        assert!(!result_xml.contains("moveTo"));
        assert!(!result_xml.contains("moved from here"));
        assert!(result_xml.contains("moved to here"));
    }

    #[test]
    fn normalize_recurses_into_sdt() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:sdt>
      <w:sdtContent>
        <w:p>
          <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
            <w:r><w:t>inside sdt</w:t></w:r>
          </w:ins>
        </w:p>
      </w:sdtContent>
    </w:sdt>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = normalize_docx(&archive).unwrap();
        assert_eq!(stats.opaque_nodes_resolved_revisions_count, 1);

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();
        assert!(!result_xml.contains("w:ins"));
        assert!(result_xml.contains("inside sdt"));
    }

    #[test]
    fn resolution_may_empty_a_block_content_control() {
        let deleted = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:sdt><w:sdtContent><w:p>
      <w:pPr><w:rPr><w:del w:id="1" w:author="A"/></w:rPr></w:pPr>
      <w:del w:id="2" w:author="A"><w:r><w:delText>gone</w:delText></w:r></w:del>
    </w:p></w:sdtContent></w:sdt>
    <w:p><w:r><w:t>survives</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let inserted = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:sdt><w:sdtContent><w:p>
      <w:pPr><w:rPr><w:ins w:id="1" w:author="A"/></w:rPr></w:pPr>
      <w:ins w:id="2" w:author="A"><w:r><w:t>gone</w:t></w:r></w:ins>
    </w:p></w:sdtContent></w:sdt>
    <w:p><w:r><w:t>survives</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let (accepted, _) = normalize_docx(&archive_with_document_xml(deleted))
            .expect("accept deleted wrapped paragraph");
        let (rejected, _) = reject_all_docx(&archive_with_document_xml(inserted))
            .expect("reject inserted wrapped paragraph");
        for resolved in [accepted, rejected] {
            let xml = std::str::from_utf8(resolved.get("word/document.xml").unwrap()).unwrap();
            assert!(xml.contains("<w:sdtContent"));
            assert!(!xml.contains("gone"));
            assert_eq!(xml.matches("<w:p>").count(), 1);
            assert!(xml.contains("survives"));
        }
    }

    #[test]
    fn normalize_handles_nested_revisions() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r>
          <w:rPr>
            <w:b/>
            <w:rPrChange w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z">
              <w:rPr/>
            </w:rPrChange>
          </w:rPr>
          <w:t>bold inserted</w:t>
        </w:r>
      </w:ins>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = normalize_docx(&archive).unwrap();
        // 1 for w:ins + 1 for w:rPrChange
        assert_eq!(stats.revisions_resolved, 2);

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();
        assert!(!result_xml.contains("w:ins"));
        assert!(!result_xml.contains("rPrChange"));
        assert!(result_xml.contains("bold inserted"));
        // The rPr with b should remain
        assert!(result_xml.contains("<w:b"));
    }

    #[test]
    fn normalize_multipart_with_header() {
        let doc_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>body</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let header_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:p>
    <w:del w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
      <w:r><w:delText>old header</w:delText></w:r>
    </w:del>
  </w:p>
</w:hdr>"#;

        let rels_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#;

        let archive = archive_with_parts(vec![
            ("word/document.xml", doc_xml),
            ("word/header1.xml", header_xml),
            ("word/_rels/document.xml.rels", rels_xml),
        ]);

        let (result_archive, stats) = normalize_docx(&archive).unwrap();
        assert!(stats.revisions_resolved >= 1);
        assert!(
            stats
                .parts_normalized
                .contains(&"word/header1.xml".to_string())
        );

        let result_header =
            std::str::from_utf8(result_archive.get("word/header1.xml").unwrap()).unwrap();
        assert!(!result_header.contains("w:del"));
        assert!(!result_header.contains("old header"));
    }

    #[test]
    fn normalize_zero_revisions_invariant() {
        // After normalization, scanning should find zero revision elements.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>ins1</w:t></w:r>
      </w:ins>
      <w:del w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>del1</w:delText></w:r>
      </w:del>
      <w:moveTo w:id="3" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>move target</w:t></w:r>
      </w:moveTo>
      <w:moveFrom w:id="4" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>move source</w:t></w:r>
      </w:moveFrom>
      <w:pPr>
        <w:pPrChange w:id="5" w:author="A" w:date="2024-01-01T00:00:00Z">
          <w:pPr/>
        </w:pPrChange>
      </w:pPr>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, _) = normalize_docx(&archive).unwrap();

        // Post-condition: preflight scan of result must show zero revisions
        let report = preflight_scan(&result_archive).unwrap();
        assert_eq!(
            report.totals.revisions.total(),
            0,
            "Post-normalization invariant violated: found {} revision elements",
            report.totals.revisions.total()
        );
    }

    #[test]
    fn normalize_preexisting_del_ins_produces_clean_text() {
        // Reproduces the garbled text bug: without normalization, the canonical
        // model would see "December 1, 2024January 1, 2025" instead of just
        // "January 1, 2025".
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t xml:space="preserve">Date: </w:t></w:r>
      <w:del w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>December 1, 2024</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="B" w:date="2024-02-01T00:00:00Z">
        <w:r><w:t>January 1, 2025</w:t></w:r>
      </w:ins>
    </w:p>
    <w:p>
      <w:r><w:t xml:space="preserve">Contact: </w:t></w:r>
      <w:del w:id="3" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>John Doe</w:delText></w:r>
      </w:del>
      <w:ins w:id="4" w:author="B" w:date="2024-02-01T00:00:00Z">
        <w:r><w:t>Jane Smith</w:t></w:r>
      </w:ins>
    </w:p>
    <w:p>
      <w:r><w:t xml:space="preserve">Rate: $</w:t></w:r>
      <w:del w:id="5" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>125</w:delText></w:r>
      </w:del>
      <w:ins w:id="6" w:author="B" w:date="2024-02-01T00:00:00Z">
        <w:r><w:t>150</w:t></w:r>
      </w:ins>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = normalize_docx(&archive).unwrap();

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        // Deleted text must be gone
        assert!(
            !result_xml.contains("December 1, 2024"),
            "deleted date should be removed"
        );
        assert!(
            !result_xml.contains("John Doe"),
            "deleted name should be removed"
        );
        assert!(
            !result_xml.contains("125"),
            "deleted rate should be removed"
        );

        // Inserted text must remain
        assert!(
            result_xml.contains("January 1, 2025"),
            "inserted date should remain"
        );
        assert!(
            result_xml.contains("Jane Smith"),
            "inserted name should remain"
        );
        assert!(result_xml.contains("150"), "inserted rate should remain");

        // No revision markup should remain
        assert!(!result_xml.contains("w:del"), "w:del should be removed");
        assert!(!result_xml.contains("w:ins"), "w:ins should be removed");
        assert!(!result_xml.contains("delText"), "delText should be removed");

        // All 6 revision elements resolved (3 w:del + 3 w:ins, plus nested delText)
        assert!(stats.revisions_resolved >= 6);
    }

    // -----------------------------------------------------------------------
    // Reject all tests
    // -----------------------------------------------------------------------

    #[test]
    fn note_resolution_preserves_structural_definitions_in_order() {
        // Structural notes are not body-referenced user notes. Resolving a
        // revision must preserve their content and order while still pruning
        // the user note whose reference disappears.
        for kind in ["footnote", "endnote"] {
            for (carrier, accept) in [("ins", false), ("del", true)] {
                let document = format!(
                    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:{kind}Reference w:id="8"/></w:r><w:{carrier} w:id="12" w:author="Editor"><w:r><w:{kind}Reference w:id="9"/></w:r></w:{carrier}></w:p></w:body></w:document>"#
                );
                let notes = format!(
                    r#"<w:{kind}s xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:{kind} w:id="-1" w:type="separator"><w:p/></w:{kind}><w:{kind} w:id="0" w:type="continuationSeparator"><w:p/></w:{kind}><w:{kind} w:id="7" w:type="continuationNotice"><w:p><w:r><w:t>Notes continue.</w:t></w:r></w:p></w:{kind}><w:{kind} w:id="8"><w:p><w:r><w:t>Retained.</w:t></w:r></w:p></w:{kind}><w:{kind} w:id="9"><w:p><w:r><w:t>Removed.</w:t></w:r></w:p></w:{kind}></w:{kind}s>"#
                );
                let relationships = format!(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}s" Target="{kind}s.xml"/></Relationships>"#
                );
                let path = format!("word/{kind}s.xml");
                let archive = archive_with_parts(vec![
                    ("word/document.xml", &document),
                    (&path, &notes),
                    ("word/_rels/document.xml.rels", &relationships),
                ]);
                let (resolved, _) = if accept {
                    normalize_docx(&archive).unwrap()
                } else {
                    reject_all_docx(&archive).unwrap()
                };
                let root = parse_xml(resolved.get(&path).unwrap()).unwrap();
                let ids: Vec<_> = root
                    .children
                    .iter()
                    .filter_map(|child| child.as_element().and_then(|note| get_attr(note, "id")))
                    .collect();
                assert_eq!(ids, ["-1", "0", "7", "8"], "{kind}, accept={accept}");
                let xml = std::str::from_utf8(resolved.get(&path).unwrap()).unwrap();
                assert!(xml.contains("Notes continue."));
                assert!(xml.contains("Retained."));
                assert!(!xml.contains("Removed."));
            }
        }
    }

    #[test]
    fn reject_inserted_footnote_prunes_only_its_orphaned_definition() {
        let document = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p>
    <w:r><w:footnoteReference w:id="1"/></w:r>
    <w:ins w:id="7" w:author="A"><w:r><w:footnoteReference w:id="2"/></w:r></w:ins>
  </w:p></w:body>
</w:document>"#;
        let footnotes = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnote w:type="separator" w:id="-1"><w:p/></w:footnote>
  <w:footnote w:id="1"><w:p><w:r><w:t>Existing note.</w:t></w:r></w:p></w:footnote>
  <w:footnote w:id="2"><w:p><w:r><w:t>Inserted note.</w:t></w:r></w:p></w:footnote>
</w:footnotes>"#;
        let relationships = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>
</Relationships>"#;
        let archive = archive_with_parts(vec![
            ("word/document.xml", document),
            ("word/footnotes.xml", footnotes),
            ("word/_rels/document.xml.rels", relationships),
        ]);

        let (rejected, _) = reject_all_docx(&archive).expect("reject inserted footnote");
        let rejected_main =
            std::str::from_utf8(rejected.get("word/document.xml").unwrap()).unwrap();
        let rejected_notes =
            std::str::from_utf8(rejected.get("word/footnotes.xml").unwrap()).unwrap();
        assert!(rejected_main.contains("w:id=\"1\""));
        assert!(!rejected_main.contains("w:id=\"2\""));
        assert!(rejected_notes.contains("Existing note."));
        assert!(!rejected_notes.contains("Inserted note."));
        assert!(rejected_notes.contains("separator"));
    }

    #[test]
    fn accept_deleted_endnote_prunes_its_orphaned_definition() {
        let document = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p>
    <w:del w:id="8" w:author="A"><w:r><w:endnoteReference w:id="3"/></w:r></w:del>
  </w:p></w:body>
</w:document>"#;
        let endnotes = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:endnote w:type="continuationSeparator" w:id="0"><w:p/></w:endnote>
  <w:endnote w:id="3"><w:p><w:r><w:t>Deleted note.</w:t></w:r></w:p></w:endnote>
</w:endnotes>"#;
        let relationships = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes" Target="endnotes.xml"/>
</Relationships>"#;
        let archive = archive_with_parts(vec![
            ("word/document.xml", document),
            ("word/endnotes.xml", endnotes),
            ("word/_rels/document.xml.rels", relationships),
        ]);

        let (accepted, _) = normalize_docx(&archive).expect("accept deleted endnote");
        let accepted_notes =
            std::str::from_utf8(accepted.get("word/endnotes.xml").unwrap()).unwrap();
        assert!(!accepted_notes.contains("Deleted note."));
        assert!(accepted_notes.contains("continuationSeparator"));
    }

    #[test]
    fn reject_restores_deleted_text_and_drops_insertions() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t xml:space="preserve">Date: </w:t></w:r>
      <w:del w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>December 1, 2024</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="B" w:date="2024-02-01T00:00:00Z">
        <w:r><w:t>January 1, 2025</w:t></w:r>
      </w:ins>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = reject_all_docx(&archive).unwrap();

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        // Deleted text should be restored as w:t
        assert!(
            result_xml.contains("December 1, 2024"),
            "deleted text should be restored"
        );
        assert!(
            !result_xml.contains("delText"),
            "delText should be converted to t"
        );

        // Inserted text should be gone
        assert!(
            !result_xml.contains("January 1, 2025"),
            "inserted text should be removed"
        );

        // No revision markup should remain
        assert!(!result_xml.contains("w:del"), "w:del should be removed");
        assert!(!result_xml.contains("w:ins"), "w:ins should be removed");

        assert!(stats.revisions_resolved >= 3);
    }

    #[test]
    fn reject_restores_rpr_change_previous_properties() {
        // §17.13.5.30: reject must restore the rPr recorded in rPrChange,
        // not keep the new formatting.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:rPr><w:caps/><w:rPrChange w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr>
        <w:t>Shout</w:t>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;
        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = reject_all_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        assert!(
            result_xml.contains("<w:i"),
            "previous italic must be restored"
        );
        assert!(
            !result_xml.contains("<w:caps"),
            "new caps must be removed on reject"
        );
        assert!(!result_xml.contains("rPrChange"), "record must be resolved");
        assert_eq!(stats.revisions_resolved, 1);
    }

    #[test]
    fn reject_restores_ppr_change_keeping_mark_rpr() {
        // §17.13.5.29: pPrChange's child pPr is CT_PPrBase — it carries no
        // w:rPr. Reject must restore the previous base properties while
        // KEEPING the current paragraph-mark rPr (here with bold), or the
        // mark's own state would be silently lost.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:jc w:val="center"/><w:rPr><w:b/></w:rPr><w:pPrChange w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"><w:pPr><w:jc w:val="right"/></w:pPr></w:pPrChange></w:pPr>
      <w:r><w:t>Text.</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;
        let archive = archive_with_document_xml(xml);
        let (result_archive, _) = reject_all_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        assert!(
            result_xml.contains(r#"<w:jc w:val="right""#),
            "previous alignment must be restored, got: {result_xml}"
        );
        assert!(
            !result_xml.contains(r#"w:val="center""#),
            "new alignment must go"
        );
        assert!(
            result_xml.contains("<w:b"),
            "paragraph-mark rPr must survive the restore"
        );
        assert!(!result_xml.contains("pPrChange"), "record must be resolved");
    }

    #[test]
    fn reject_trpr_change_restore_keeps_row_insertion_marker() {
        // The trPr revision markers are a SEPARATE axis from trPrChange:
        // restoring previous row properties must not erase w:trPr/w:ins, or
        // the rejected-inserted row would wrongly survive.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="4675"/></w:tblGrid>
      <w:tr><w:tc><w:p><w:r><w:t>Keep row.</w:t></w:r></w:p></w:tc></w:tr>
      <w:tr>
        <w:trPr><w:trHeight w:val="400"/><w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/><w:trPrChange w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z"><w:trPr><w:cantSplit/></w:trPr></w:trPrChange></w:trPr>
        <w:tc><w:p><w:ins w:id="3" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:t>Inserted row.</w:t></w:r></w:ins></w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:p><w:r><w:t>Tail.</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let archive = archive_with_document_xml(xml);
        let (result_archive, _) = reject_all_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        assert!(
            !result_xml.contains("Inserted row."),
            "rejected inserted row must be dropped even when trPrChange was restored"
        );
        assert!(
            result_xml.contains("Keep row."),
            "untracked row must survive"
        );
    }

    #[test]
    fn reject_tcpr_change_restore_keeps_cell_deletion_marker() {
        // Same separate-axis rule for cells: restoring tcPr from tcPrChange
        // must keep w:cellDel so the marker resolves (cell restored, marker
        // stripped) instead of leaking or losing state.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>
      <w:tr>
        <w:tc>
          <w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:cellDel w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/><w:tcPrChange w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z"><w:tcPr><w:tcW w:w="1500" w:type="dxa"/></w:tcPr></w:tcPrChange></w:tcPr>
          <w:p><w:r><w:t>Cell text.</w:t></w:r></w:p>
        </w:tc>
        <w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Other.</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;
        let archive = archive_with_document_xml(xml);
        let (result_archive, _) = reject_all_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        assert!(
            result_xml.contains("Cell text."),
            "deleted-marked cell is restored on reject"
        );
        assert!(
            result_xml.contains(r#"<w:tcW w:w="1500""#),
            "previous cell width must be restored, got: {result_xml}"
        );
        assert!(
            !result_xml.contains("cellDel"),
            "cell marker must be resolved"
        );
        assert!(
            !result_xml.contains("tcPrChange"),
            "record must be resolved"
        );
    }

    #[test]
    fn reject_removes_childless_pr_change_record() {
        // A *PrChange with no property child carries no previous state:
        // remove the record, keep the current properties (mirrors the model
        // path, which ignores such records at import).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:jc w:val="center"/><w:pPrChange w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:pPr>
      <w:r><w:t>Text.</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;
        let archive = archive_with_document_xml(xml);
        let (result_archive, _) = reject_all_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        assert!(
            result_xml.contains(r#"<w:jc w:val="center""#),
            "current properties stay when the record has no previous state"
        );
        assert!(
            !result_xml.contains("pPrChange"),
            "record must still be resolved"
        );
    }

    #[test]
    fn reject_sect_pr_change_restore_keeps_header_references() {
        // §17.13.5.32: sectPrChange's child sectPr is CT_SectPrBase — no
        // header/footer references. Restoring must keep the current ones.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:t>Body.</w:t></w:r></w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId4"/>
      <w:pgSz w:w="11906" w:h="16838"/>
      <w:sectPrChange w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:sectPrChange>
    </w:sectPr>
  </w:body>
</w:document>"#;
        let archive = archive_with_document_xml(xml);
        let (result_archive, _) = reject_all_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        assert!(
            result_xml.contains(r#"<w:pgSz w:w="12240""#),
            "previous page size must be restored, got: {result_xml}"
        );
        assert!(
            !result_xml.contains(r#"w:w="11906""#),
            "new page size must go"
        );
        assert!(
            result_xml.contains("headerReference"),
            "header reference must survive the restore"
        );
        assert!(
            !result_xml.contains("sectPrChange"),
            "record must be resolved"
        );
    }

    #[test]
    fn reject_handles_move_from_to() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:moveFrom w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>moved from here</w:t></w:r>
      </w:moveFrom>
      <w:moveTo w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>moved to here</w:t></w:r>
      </w:moveTo>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, stats) = reject_all_docx(&archive).unwrap();

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        // moveFrom should be unwrapped (content restored)
        assert!(
            result_xml.contains("moved from here"),
            "moveFrom content should be kept"
        );
        // moveTo should be dropped (move target rejected)
        assert!(
            !result_xml.contains("moved to here"),
            "moveTo content should be removed"
        );
        assert!(!result_xml.contains("moveFrom"));
        assert!(!result_xml.contains("moveTo"));

        assert!(stats.revisions_resolved >= 2);
    }

    #[test]
    fn reject_move_restores_an_insertion_nested_at_the_source() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p>
    <w:moveFrom w:id="2" w:author="Mover">
      <w:ins w:id="1" w:author="Earlier"><w:r><w:t>inserted then moved</w:t></w:r></w:ins>
    </w:moveFrom>
    <w:moveTo w:id="3" w:author="Mover"><w:r><w:t>inserted then moved</w:t></w:r></w:moveTo>
  </w:p></w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (rejected, _) = reject_all_docx(&archive).expect("reject nested move source");
        let xml = std::str::from_utf8(rejected.get("word/document.xml").unwrap()).unwrap();
        assert!(xml.contains("inserted then moved"));
        assert!(!xml.contains("<w:ins") && !xml.contains("<w:moveFrom"));
        assert_eq!(xml.matches("inserted then moved").count(), 1);
    }

    #[test]
    fn reject_zero_revisions_invariant() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>ins1</w:t></w:r>
      </w:ins>
      <w:del w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>del1</w:delText></w:r>
      </w:del>
      <w:moveTo w:id="3" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>move target</w:t></w:r>
      </w:moveTo>
      <w:moveFrom w:id="4" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>move source</w:t></w:r>
      </w:moveFrom>
      <w:pPr>
        <w:pPrChange w:id="5" w:author="A" w:date="2024-01-01T00:00:00Z">
          <w:pPr/>
        </w:pPrChange>
      </w:pPr>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, _) = reject_all_docx(&archive).unwrap();

        let report = preflight_scan(&result_archive).unwrap();
        assert_eq!(
            report.totals.revisions.total(),
            0,
            "Post-reject invariant violated: found {} revision elements",
            report.totals.revisions.total()
        );
    }

    // -----------------------------------------------------------------------
    // Row-level tracked insertion/deletion tests
    // -----------------------------------------------------------------------

    #[test]
    fn reject_drops_row_with_trpr_ins() {
        // A table row marked as inserted via w:trPr/w:ins should be entirely
        // removed on reject — the row didn't exist before the tracked change.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>existing row</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:trPr>
          <w:ins w:id="1" w:author="A" />
        </w:trPr>
        <w:tc><w:p>
          <w:ins w:id="2" w:author="A">
            <w:r><w:t>inserted row</w:t></w:r>
          </w:ins>
        </w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>another existing row</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, _stats) = reject_all_docx(&archive).unwrap();

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        // The inserted row should be gone entirely
        assert!(
            !result_xml.contains("inserted row"),
            "inserted row text should be removed"
        );
        assert!(
            result_xml.contains("existing row"),
            "existing rows should remain"
        );
        assert!(
            result_xml.contains("another existing row"),
            "existing rows should remain"
        );

        // Only the 2 existing rows remain
        let tr_count = result_xml.matches("<w:tr>").count() + result_xml.matches("<w:tr ").count();
        assert_eq!(tr_count, 2, "should have 2 rows (inserted row dropped)");
    }

    #[test]
    fn accept_removes_trpr_del_row() {
        // ECMA-376 §17.13.5.12: a w:trPr/w:del marks the ENTIRE row as a tracked
        // deletion. Accepting a deletion yields the document as if the deleted
        // content were never present, so the whole w:tr (and all its cell
        // content, including any inline w:ins replacement that lived inside the
        // deleted row) must be removed. The non-deleted row survives.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>kept row</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:trPr>
          <w:del w:id="1" w:author="A" />
        </w:trPr>
        <w:tc><w:p>
          <w:del w:id="2" w:author="A">
            <w:r><w:delText>deleted content</w:delText></w:r>
          </w:del>
          <w:ins w:id="3" w:author="A">
            <w:r><w:t>replacement content</w:t></w:r>
          </w:ins>
        </w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, _stats) = normalize_docx(&archive).unwrap();

        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        // The whole deleted row is gone — its content (including any inline
        // replacement that lived inside the deleted row) disappears with it.
        assert!(
            !result_xml.contains("deleted content"),
            "deleted content should be removed on accept"
        );
        assert!(
            !result_xml.contains("replacement content"),
            "content inside the deleted row is removed along with the row"
        );
        assert!(
            result_xml.contains("kept row"),
            "non-deleted row should remain"
        );

        // Only the non-deleted row remains (ECMA-376 §17.13.5.12).
        let tr_count = result_xml.matches("<w:tr>").count() + result_xml.matches("<w:tr ").count();
        assert_eq!(
            tr_count, 1,
            "the trPr/w:del row is removed on accept (§17.13.5.12)"
        );

        // The w:trPr/w:del marker should be stripped
        assert!(
            !result_xml.contains("<w:del"),
            "tracking markers should be stripped"
        );
    }

    // -----------------------------------------------------------------------
    // Empty table removal tests (OOXML §17.4.37 invariant)
    // -----------------------------------------------------------------------

    #[test]
    fn reject_removes_table_when_all_rows_are_inserted() {
        // When every row has w:trPr/w:ins, rejecting drops all rows.
        // The now-empty table must also be removed (OOXML §17.4.37: non-zero rows).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>before table</w:t></w:r></w:p>
    <w:tbl>
      <w:tblPr><w:tblW w:w="5000" w:type="dxa" /></w:tblPr>
      <w:tblGrid><w:gridCol w:w="2500" /><w:gridCol w:w="2500" /></w:tblGrid>
      <w:tr>
        <w:trPr><w:ins w:id="1" w:author="A" /></w:trPr>
        <w:tc><w:p><w:ins w:id="2" w:author="A"><w:r><w:t>cell A</w:t></w:r></w:ins></w:p></w:tc>
        <w:tc><w:p><w:ins w:id="3" w:author="A"><w:r><w:t>cell B</w:t></w:r></w:ins></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:trPr><w:ins w:id="4" w:author="A" /></w:trPr>
        <w:tc><w:p><w:ins w:id="5" w:author="A"><w:r><w:t>cell C</w:t></w:r></w:ins></w:p></w:tc>
        <w:tc><w:p><w:ins w:id="6" w:author="A"><w:r><w:t>cell D</w:t></w:r></w:ins></w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:p><w:r><w:t>after table</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, _stats) = reject_all_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        // The entire table should be gone — no tbl element
        assert!(
            !result_xml.contains("<w:tbl"),
            "empty table should be removed entirely, got: {result_xml}"
        );
        // Surrounding content should remain
        assert!(result_xml.contains("before table"));
        assert!(result_xml.contains("after table"));
    }

    #[test]
    fn accept_removes_table_when_all_rows_are_deleted() {
        // ECMA-376 §17.13.5.12: each w:trPr/w:del row is a tracked deletion of
        // the whole row, so accepting removes every row. The now-empty table is
        // invalid (OOXML §17.4.37: a table must have a non-zero number of rows),
        // so it is removed entirely — mirroring the reject path for all-inserted
        // rows. Surrounding content is unaffected.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>before table</w:t></w:r></w:p>
    <w:tbl>
      <w:tblPr><w:tblW w:w="5000" w:type="dxa" /></w:tblPr>
      <w:tblGrid><w:gridCol w:w="2500" /><w:gridCol w:w="2500" /></w:tblGrid>
      <w:tr>
        <w:trPr><w:del w:id="1" w:author="A" /></w:trPr>
        <w:tc><w:p>
          <w:del w:id="3" w:author="A"><w:r><w:delText>old cell A</w:delText></w:r></w:del>
          <w:ins w:id="4" w:author="A"><w:r><w:t>new cell A</w:t></w:r></w:ins>
        </w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:trPr><w:del w:id="2" w:author="A" /></w:trPr>
        <w:tc><w:p>
          <w:del w:id="5" w:author="A"><w:r><w:delText>old cell B</w:delText></w:r></w:del>
          <w:ins w:id="6" w:author="A"><w:r><w:t>new cell B</w:t></w:r></w:ins>
        </w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:p><w:r><w:t>after table</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, _stats) = normalize_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        // Every row was a tracked deletion → all rows removed → empty table
        // dropped entirely (OOXML §17.4.37).
        assert!(
            !result_xml.contains("<w:tbl"),
            "table with all rows deleted should be removed entirely, got: {result_xml}"
        );
        let tr_count = result_xml.matches("<w:tr>").count() + result_xml.matches("<w:tr ").count();
        assert_eq!(tr_count, 0, "no rows should remain (all were deleted)");

        // Deleted-row content (both the w:delText and the inline w:ins inside
        // the deleted rows) is gone along with the rows.
        assert!(
            !result_xml.contains("old cell A"),
            "deleted content should be removed"
        );
        assert!(
            !result_xml.contains("new cell A"),
            "content inside deleted rows is removed along with the rows"
        );
        assert!(
            !result_xml.contains("<w:del"),
            "tracking markers should be stripped"
        );

        // Surrounding content should remain
        assert!(result_xml.contains("before table"));
        assert!(result_xml.contains("after table"));
    }

    #[test]
    fn reject_keeps_table_when_some_rows_remain() {
        // When only some rows are tracked insertions, the table should survive
        // with the remaining rows intact.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>existing row</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:trPr><w:ins w:id="1" w:author="A" /></w:trPr>
        <w:tc><w:p><w:ins w:id="2" w:author="A"><w:r><w:t>inserted row</w:t></w:r></w:ins></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let (result_archive, _stats) = reject_all_docx(&archive).unwrap();
        let result_xml =
            std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

        // Table should remain (it still has 1 row)
        assert!(
            result_xml.contains("<w:tbl"),
            "table with remaining rows should be kept"
        );
        assert!(result_xml.contains("existing row"));
        assert!(!result_xml.contains("inserted row"));
    }

    // -----------------------------------------------------------------------
    // Normalize-if-needed tests
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_if_needed_skips_clean_document() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>No tracked changes here.</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let result = normalize_if_needed(&archive).unwrap();
        let result_xml = std::str::from_utf8(result.get("word/document.xml").unwrap()).unwrap();
        assert!(result_xml.contains("No tracked changes here."));
    }

    #[test]
    fn normalize_if_needed_accepts_changes_when_present() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:del w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>old</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="B" w:date="2024-02-01T00:00:00Z">
        <w:r><w:t>new</w:t></w:r>
      </w:ins>
    </w:p>
  </w:body>
</w:document>"#;

        let archive = archive_with_document_xml(xml);
        let result = normalize_if_needed(&archive).unwrap();
        let result_xml = std::str::from_utf8(result.get("word/document.xml").unwrap()).unwrap();
        assert!(
            !result_xml.contains("old"),
            "deleted text should be removed"
        );
        assert!(result_xml.contains("new"), "inserted text should remain");
        assert!(!result_xml.contains("w:del"));
        assert!(!result_xml.contains("w:ins"));
    }

    // ── resolve_opaque_fragment_revisions (M0.1 fragment resolver) ──────────

    #[test]
    fn resolving_all_revisions_resolves_every_drawing_representation() {
        // A compatibility drawing's representations show one shape. Accepting
        // or rejecting everything resolves each the same way, so none is left
        // with pending revisions; the package resolver does the same. A
        // selection names revisions of the active representation only: an
        // inactive carrier cannot be matched to its active counterpart.
        let raw = br#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><mc:Choice Requires="wps"><w:drawing><wps:txbx><w:txbxContent><w:p><w:ins w:id="7" w:author="Active"><w:r><w:t>Current</w:t></w:r></w:ins></w:p></w:txbxContent></wps:txbx></w:drawing></mc:Choice><mc:Fallback><w:pict><w:txbxContent><w:p><w:ins w:id="99" w:author="Legacy"><w:r><w:t>Legacy</w:t></w:r></w:ins></w:p></w:txbxContent></w:pict></mc:Fallback></mc:AlternateContent>"#;
        let original = crate::word_xml::parse_raw_fragment(raw).unwrap();
        let fallback =
            crate::word_xml::serialize_raw_fragment(original.get_child("Fallback").unwrap());
        assert_eq!(crate::tracked_model::max_interior_id(raw), 7);
        let fallback_of = |bytes: &[u8]| {
            let resolved = crate::word_xml::parse_raw_fragment(bytes).unwrap();
            String::from_utf8(crate::word_xml::serialize_raw_fragment(
                resolved.get_child("Fallback").unwrap(),
            ))
            .unwrap()
        };
        let active_of = |bytes: &[u8]| {
            let fragment = crate::word_xml::OpaqueFragment::parse(bytes).unwrap();
            String::from_utf8(crate::word_xml::serialize_raw_fragment(fragment.active())).unwrap()
        };
        for accept in [false, true] {
            let FragmentResolution::Resolved(all) = resolve_opaque_fragment_revisions(raw, accept)
            else {
                panic!("revisions must resolve");
            };
            let resolved_fallback = fallback_of(&all);
            assert!(!resolved_fallback.contains("w:ins"), "{resolved_fallback}");
            assert_eq!(resolved_fallback.contains("Legacy"), accept);
            assert_eq!(active_of(&all).contains("Current"), accept);
            assert_eq!(
                resolve_opaque_fragment_revisions(&all, accept),
                FragmentResolution::Clean
            );

            let FragmentResolution::Resolved(selected) =
                resolve_fragment_selected(raw, &std::collections::HashSet::from([7]), accept)
            else {
                panic!("the active revision must resolve");
            };
            assert_eq!(
                fallback_of(&selected).as_bytes(),
                fallback.as_slice(),
                "a selection leaves the inactive representation as it was"
            );
            assert_eq!(active_of(&selected).contains("Current"), accept);
            assert_eq!(crate::tracked_model::max_interior_id(&selected), 0);
        }
        assert_eq!(
            resolve_fragment_selected(raw, &std::collections::HashSet::from([99]), true),
            FragmentResolution::Clean,
            "inactive carrier is not a selectable current revision"
        );
    }

    #[test]
    fn fragment_resolver_resolves_revisions() {
        // A parseable sdt fragment with a w:ins → Resolved, with the wrapper
        // unwrapped on accept.
        let raw = br#"<w:sdt><w:sdtContent><w:r><w:t>OLD</w:t></w:r><w:ins w:id="1" w:author="a" w:date="2026-06-11T00:00:00Z"><w:r><w:t> NEW</w:t></w:r></w:ins></w:sdtContent></w:sdt>"#;
        match resolve_opaque_fragment_revisions(raw, /*keep_inserted=*/ true) {
            FragmentResolution::Resolved(bytes) => {
                let s = String::from_utf8(bytes).unwrap();
                assert!(!s.contains("<w:ins"), "accept unwraps the ins: {s}");
                assert!(s.contains("OLD") && s.contains("NEW"), "{s}");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    fn resolved_text(raw: &[u8], accept: bool) -> String {
        match resolve_opaque_fragment_revisions(raw, accept) {
            FragmentResolution::Resolved(bytes) => String::from_utf8(bytes).unwrap(),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    /// One run split around a tracked replacement is one run again once the
    /// change is resolved either way: the pieces have the same properties and
    /// meet where the change was, so the source's spelling is the one spelling.
    #[test]
    fn resolving_a_change_inside_a_run_rejoins_the_run() {
        let split = br#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Keep </w:t></w:r><w:del w:id="1" w:author="a"><w:r><w:rPr><w:b/></w:rPr><w:delText>old</w:delText></w:r></w:del><w:ins w:id="2" w:author="a"><w:r><w:rPr><w:b/></w:rPr><w:t>new</w:t></w:r></w:ins><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve"> words</w:t></w:r></w:p>"#;
        let rejected = resolved_text(split, false);
        assert_eq!(rejected.matches("<w:r>").count(), 1, "{rejected}");
        assert!(rejected.contains("<w:t>Keep old words</w:t>"), "{rejected}");
        let accepted = resolved_text(split, true);
        assert_eq!(accepted.matches("<w:r>").count(), 1, "{accepted}");
        assert!(accepted.contains("<w:t>Keep new words</w:t>"), "{accepted}");
    }

    /// Only the runs meeting at the change are joined: runs the source kept
    /// apart, or that differ in formatting, stay as they are, and joined text
    /// with edge whitespace keeps `xml:space="preserve"`.
    #[test]
    fn resolving_a_change_joins_only_matching_runs_at_the_change() {
        let raw = br#"<w:p><w:r><w:t>One</w:t></w:r><w:r><w:t xml:space="preserve"> two </w:t></w:r><w:ins w:id="1" w:author="a"><w:r><w:t>three</w:t></w:r></w:ins><w:r><w:rPr><w:i/></w:rPr><w:t>four</w:t></w:r></w:p>"#;
        let rejected = resolved_text(raw, false);
        assert!(rejected.contains("<w:t>One</w:t>"), "{rejected}");
        assert!(
            rejected.contains(r#"<w:t xml:space="preserve"> two </w:t>"#),
            "{rejected}"
        );
        let accepted = resolved_text(raw, true);
        assert!(accepted.contains("<w:t>One</w:t>"), "{accepted}");
        assert!(
            accepted.contains(r#"<w:t xml:space="preserve"> two three</w:t>"#),
            "{accepted}"
        );
        assert!(accepted.contains("<w:t>four</w:t>"), "{accepted}");
    }

    #[test]
    fn fragment_resolver_clean_when_no_revisions() {
        // A parseable fragment with no revisions → Clean (left byte-verbatim).
        let raw = br#"<w:sdt><w:sdtContent><w:r><w:t>OLD NEW</w:t></w:r></w:sdtContent></w:sdt>"#;
        assert_eq!(
            resolve_opaque_fragment_revisions(raw, true),
            FragmentResolution::Clean
        );
    }

    #[test]
    fn fragment_resolver_refuses_unparseable_with_revisions() {
        // Malformed XML (unclosed tag) that nonetheless carries a revision marker
        // → UnparseableWithRevisions. The caller (project) must refuse rather
        // than silently leave the revision unresolved.
        let raw =
            br#"<w:sdt><w:sdtContent><w:ins w:id="1"><w:r><w:t>NEW</w:t></w:sdtContent></w:sdt>"#;
        assert_eq!(
            resolve_opaque_fragment_revisions(raw, true),
            FragmentResolution::UnparseableWithRevisions
        );
    }

    #[test]
    fn fragment_resolver_clean_when_unparseable_without_revisions() {
        // Malformed XML with NO revision marker → Clean (nothing to resolve;
        // leave verbatim, the byte path stays the authority).
        let raw = br#"<w:sdt><w:sdtContent><w:r><w:t>NEW</w:sdtContent></w:sdt>"#;
        assert_eq!(
            resolve_opaque_fragment_revisions(raw, true),
            FragmentResolution::Clean
        );
    }

    /// A 4-column table (2000/2500/1500/3000) under `table_properties`
    /// whose column 1 is tracked as deleted, its cells holding `deleted`.
    fn column_deletion_document(table_properties: &str, deleted: &str) -> String {
        let rev = r#"w:author="A" w:date="2024-01-01T00:00:00Z""#;
        let cell = |width: u32, content: &str, marker: &str| {
            format!(
                r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{marker}</w:tcPr>{content}</w:tc>"#
            )
        };
        let plain = |text: &str| format!(r#"<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"#);
        let mut rows = String::new();
        for (row, id) in [("r0", 10), ("r1", 20)] {
            rows.push_str("<w:tr>");
            rows.push_str(&cell(2000, &plain(&format!("{row} item")), ""));
            rows.push_str(&cell(
                2500,
                &deleted
                    .replace("{row}", row)
                    .replace("{id}", &id.to_string()),
                &format!(r#"<w:cellDel w:id="{}" {rev}/>"#, id + 5),
            ));
            rows.push_str(&cell(1500, &plain(&format!("{row} qty")), ""));
            rows.push_str(&cell(3000, &plain(&format!("{row} notes")), ""));
            rows.push_str("</w:tr>");
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr>{table_properties}</w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2500"/><w:gridCol w:w="1500"/><w:gridCol w:w="3000"/></w:tblGrid>{rows}</w:tbl><w:p/></w:body></w:document>"#
        )
    }

    const DELETED_WHOLE: &str = r#"<w:p><w:pPr><w:rPr><w:del w:id="{id}" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr><w:del w:id="1{id}" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:delText>{row} supplier</w:delText></w:r></w:del></w:p>"#;
    const UNTRACKED_CONTENT: &str = r#"<w:p><w:r><w:t>{row} supplier</w:t></w:r></w:p>"#;

    /// The accepted grid widths and every cell width of the document.
    fn accepted_widths(xml: &str) -> (Vec<String>, Vec<String>) {
        let (archive, _) = normalize_docx(&archive_with_document_xml(xml)).expect("accept");
        let result = std::str::from_utf8(archive.get("word/document.xml").unwrap()).unwrap();
        let values = |tag: &str| -> Vec<String> {
            result
                .match_indices(&format!("<w:{tag} w:w=\""))
                .map(|(at, found)| {
                    let rest = &result[at + found.len()..];
                    rest[..rest.find('"').unwrap()].to_string()
                })
                .collect()
        };
        (values("gridCol"), values("tcW"))
    }

    /// Byte path: a fixed table whose preferred width is the survivors'
    /// sum, its column deleted whole, keeps the survivors' widths (Word's
    /// Accept: 2000/1500/3000).
    #[test]
    fn accepting_a_column_deleted_whole_keeps_the_survivors_widths() {
        let (grid, cells) = accepted_widths(&column_deletion_document(
            r#"<w:tblW w:w="6500" w:type="dxa"/><w:tblLayout w:type="fixed"/>"#,
            DELETED_WHOLE,
        ));
        assert_eq!(grid, ["2000", "1500", "3000"]);
        assert_eq!(cells, ["2000", "1500", "3000", "2000", "1500", "3000"]);
    }

    /// Byte path: auto width, the column deleted whole: no transfer.
    #[test]
    fn accepting_an_auto_width_column_deleted_whole_keeps_the_survivors_widths() {
        let (grid, _) = accepted_widths(&column_deletion_document(
            r#"<w:tblW w:w="0" w:type="auto"/>"#,
            DELETED_WHOLE,
        ));
        assert_eq!(grid, ["2000", "1500", "3000"]);
    }

    /// Byte path: a deleted cell whose content is untracked merges into the
    /// preceding cell, which takes its width (Word's Accept: 4500).
    #[test]
    fn accepting_a_deleted_cell_with_untracked_content_widens_the_preceding_cell() {
        let (grid, cells) = accepted_widths(&column_deletion_document(
            r#"<w:tblW w:w="0" w:type="auto"/>"#,
            UNTRACKED_CONTENT,
        ));
        assert_eq!(grid, ["4500", "1500", "3000"]);
        assert_eq!(cells, ["4500", "1500", "3000", "4500", "1500", "3000"]);
    }
}
