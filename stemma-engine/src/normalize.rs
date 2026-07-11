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
    /// The package's main document part could not be located (OPC §9.3): a
    /// malformed `_rels/.rels`, a missing officeDocument relationship, an
    /// External target, or a dangling target. A real defect — normalizing the
    /// wrong part would silently corrupt output — so it is propagated, never
    /// defaulted away.
    Package(crate::docx_package::PackageError),
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
/// Not included: the glossary document part (`word/glossary/document.xml`).
/// The import/model path does not parse glossary content into tracked blocks,
/// so the model resolution path leaves glossary revisions untouched; the byte
/// path stays consistent with it. Resolving glossary revisions in both paths is
/// a separate, deliberate extension, not something to add silently here.
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

    Ok(PreflightReport {
        parts,
        totals,
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
        "rPrChange" | "pPrChange" | "tblPrChange" | "trPrChange" | "tcPrChange"
        | "sectPrChange" => rev_counts.format_pr_change += 1,
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
    let mut output = archive.clone();
    let mut result = NormalizationResult::default();

    let part_paths = collect_normalizable_part_paths(archive)?;

    for part_path in &part_paths {
        let Some(xml_bytes) = output.get(part_path) else {
            continue;
        };

        let mut root = parse_xml(xml_bytes)?;
        let mut stats = NormalizeStats::default();

        let range_markers_before = capture_tree_range_markers(&root);
        normalize_children(&mut root, &mut stats, false);
        collapse_torn_range_markers_in_tree(&mut root, &range_markers_before);

        let new_bytes = write_xml(&root)?;
        // Safe to unwrap: we just read this part, so it exists.
        output.upsert(part_path, new_bytes);

        if stats.revisions_resolved > 0 || stats.opaque_resolved > 0 {
            result.parts_normalized.push(part_path.clone());
        }
        result.revisions_resolved += stats.revisions_resolved;
        result.opaque_nodes_resolved_revisions_count += stats.opaque_resolved;
    }

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
    let mut output = archive.clone();
    let mut result = NormalizationResult::default();

    let part_paths = collect_normalizable_part_paths(archive)?;

    for part_path in &part_paths {
        let Some(xml_bytes) = output.get(part_path) else {
            continue;
        };

        let mut root = parse_xml(xml_bytes)?;
        let mut stats = NormalizeStats::default();

        let range_markers_before = capture_tree_range_markers(&root);
        reject_children(&mut root, &mut stats, false);
        collapse_torn_range_markers_in_tree(&mut root, &range_markers_before);

        let new_bytes = write_xml(&root)?;
        output.upsert(part_path, new_bytes);

        if stats.revisions_resolved > 0 || stats.opaque_resolved > 0 {
            result.parts_normalized.push(part_path.clone());
        }
        result.revisions_resolved += stats.revisions_resolved;
        result.opaque_nodes_resolved_revisions_count += stats.opaque_resolved;
    }

    Ok((output, result))
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
    /// The fragment failed to reparse BUT its raw bytes contain a revision marker
    /// (`<w:ins`/`<w:del`/`w:moveFrom`/`w:moveTo`). We cannot resolve revisions
    /// we cannot parse — and silently leaving them is the exact silent-fallback
    /// this descent exists to kill — so the projection must REFUSE rather than
    /// emit a document that still carries pending revisions it claimed to resolve.
    UnparseableWithRevisions,
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
    let mut root = match crate::word_xml::parse_raw_fragment(raw) {
        Ok(root) => root,
        Err(_) => {
            // Can't parse it. If the raw bytes carry a revision marker, the
            // descent would silently leave a pending revision unresolved —
            // refuse instead (mirrors the quarantine guard's "revisions
            // invisible to this operation" precedent). No markers → nothing to
            // resolve, leave verbatim.
            if REVISION_BYTE_MARKERS.iter().any(|m| memchr_find(raw, m)) {
                return FragmentResolution::UnparseableWithRevisions;
            }
            return FragmentResolution::Clean;
        }
    };
    let mut stats = NormalizeStats::default();
    // The root element IS the opaque container, so descend with
    // `inside_opaque = true` from the start — its inner revisions count in the
    // opaque bucket, exactly as the whole-document path tallies them when it
    // recurses through `is_opaque_container`.
    if keep_inserted {
        normalize_children(&mut root, &mut stats, true);
    } else {
        reject_children(&mut root, &mut stats, true);
    }
    if stats.opaque_resolved == 0 && stats.revisions_resolved == 0 {
        return FragmentResolution::Clean;
    }
    FragmentResolution::Resolved(crate::word_xml::serialize_raw_fragment(&root))
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
    let mut root = match crate::word_xml::parse_raw_fragment(raw) {
        Ok(root) => root,
        Err(_) => {
            if REVISION_BYTE_MARKERS.iter().any(|m| memchr_find(raw, m)) {
                return FragmentResolution::UnparseableWithRevisions;
            }
            return FragmentResolution::Clean;
        }
    };
    let mut resolved = 0usize;
    resolve_selected_in(&mut root, selected, accept, &mut resolved);
    if resolved == 0 {
        FragmentResolution::Clean
    } else {
        FragmentResolution::Resolved(crate::word_xml::serialize_raw_fragment(&root))
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
                if !is_content_revision(&el) {
                    resolve_selected_in(&mut el, selected, accept, resolved);
                }
                new_children.push(XMLNode::Element(el));
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
    // Join paragraphs whose mark insertion this reject un-proposes (must
    // precede the revision pass, which drops the markers).
    join_mark_resolved_paragraphs(parent, /*keep_inserted=*/ false, stats);

    // First pass: reject *PrChange records — restore the previous
    // properties they carry (§17.13.5.29–.32; accept drops the record and
    // keeps the new properties instead).
    reject_pr_changes(parent, stats, inside_opaque);

    // Second pass: handle content revisions with inverted logic.
    let mut new_children: Vec<XMLNode> = Vec::with_capacity(parent.children.len());

    for child in parent.children.drain(..) {
        match child {
            XMLNode::Element(el) if is_w_tag(&el, "ins") || is_w_tag(&el, "moveTo") => {
                // w:ins, w:moveTo → drop entirely (reject additions)
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
                // Recursively reject children before promoting them
                reject_children(&mut el, stats, inside_opaque);
                // Promote all children of the revision wrapper
                new_children.extend(el.children);
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
            XMLNode::Element(el) if is_w_tag(&el, "tr") && has_row_tracking(&el, "ins") => {
                // Row-level tracked insertion (w:trPr/w:ins): drop the entire
                // row on reject. The row was added as a tracked change — undoing
                // that means removing it entirely.
                let count = 1 + count_nested_revisions(&el);
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
                let child_opaque = inside_opaque || is_opaque_container(&el);
                reject_children(&mut el, stats, child_opaque);
                // A surviving cell's w:cellDel marker (§17.13.5.1) is resolved
                // on reject: the cell is restored, the marker goes.
                if is_w_tag(&el, "tc") {
                    let stripped = strip_cell_tracking_markers(&mut el);
                    if inside_opaque {
                        stats.opaque_resolved += stripped;
                    } else {
                        stats.revisions_resolved += stripped;
                    }
                }
                // OOXML §17.4.37: a table must contain a non-zero number of rows.
                // After rejection drops all inserted rows, a table can end up with
                // zero w:tr children. Remove it rather than producing invalid XML.
                if is_w_tag(&el, "tbl") && !has_any_row(&el) {
                    // Table is now empty — drop it entirely.
                } else {
                    new_children.push(XMLNode::Element(el));
                }
            }
            other => {
                new_children.push(other);
            }
        }
    }

    parent.children = new_children;
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
// range is a start/end pair joined by a part-local id; a resolution that removes
// the content holding one half while the other survives tears the pair
// (schema-invalid, Word repairs on open). Rather than drop the survivor too
// (deleting base content no revision proposed removing), we re-insert the
// removed half adjacent to the survivor, collapsing the range to a point — what
// Word does when a bookmarked range's interior is deleted. Only pairs WHOLE
// before this resolution are repaired; a half already lone in the input is the
// document's own state and passes through untouched.

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
    b"<w:trPrChange",
    b"<w:tcPrChange",
    b"<w:sectPrChange",
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
    "trPrChange",
    "tcPrChange",
    "sectPrChange",
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
        let (normalized, _) = normalize_docx(archive)?;
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
/// The model path's `merge_marked_paragraphs_*` (tracked_model.rs) applies
/// the same rule to zero-width marker OpaqueBlocks; the two lists must
/// describe the same elements or the paths diverge on which joins happen.
fn is_zero_width_body_marker(element: &Element) -> bool {
    [
        "bookmarkStart",
        "bookmarkEnd",
        "commentRangeStart",
        "commentRangeEnd",
        "proofErr",
        "permStart",
        "permEnd",
        "moveFromRangeStart",
        "moveFromRangeEnd",
        "moveToRangeStart",
        "moveToRangeEnd",
        "customXmlInsRangeStart",
        "customXmlInsRangeEnd",
        "customXmlDelRangeStart",
        "customXmlDelRangeEnd",
        "customXmlMoveFromRangeStart",
        "customXmlMoveFromRangeEnd",
        "customXmlMoveToRangeStart",
        "customXmlMoveToRangeEnd",
    ]
    .iter()
    .any(|tag| is_w_tag(element, tag))
}

/// A table this resolution empties COMPLETELY: it has rows and every one
/// carries the drop-class row marker (`w:trPr/w:del` on accept,
/// `w:trPr/w:ins` on reject — a STACKED row carries both and so drops in
/// both full resolutions). The revision pass then removes the rowless shell
/// (§17.4.37), so a paragraph-mark join must treat the table as absent.
/// Byte-level counterpart of the model path's `table_emptied_by_accept_reject`
/// (tracked_model.rs). Per-row survival goes through the shared
/// `resolution_rules::tracked_class_survives` (a row's `(has_ins, has_del)`
/// come from its `w:trPr/w:ins` / `w:trPr/w:del` markers), the same rule the
/// model's `row_survives_accept_reject` consults — so the two cannot diverge
/// on which joins step over a vanishing table.
fn table_emptied_by_resolution(tbl: &Element, keep_inserted: bool) -> bool {
    let mut saw_row = false;
    for c in &tbl.children {
        if let XMLNode::Element(el) = c
            && is_w_tag(el, "tr")
        {
            saw_row = true;
            let has_ins = has_row_tracking(el, "ins");
            let has_del = has_row_tracking(el, "del");
            if crate::resolution_rules::tracked_class_survives(has_ins, has_del, keep_inserted) {
                return false;
            }
        }
    }
    saw_row
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
        if is_zero_width_body_marker(el) || is_move_range_marker(el) {
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
            let drop_empty = match &parent.children[i] {
                XMLNode::Element(el) => {
                    paragraph_emptied_by_resolution(el, keep_inserted)
                        && parent.children[(i + 1)..].iter().any(|c| {
                            matches!(c, XMLNode::Element(el)
                                if is_w_tag(el, "p") || is_w_tag(el, "tbl"))
                        })
                }
                _ => false,
            };
            if drop_empty {
                parent.children.remove(i);
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

    for child in parent.children.drain(..) {
        match child {
            XMLNode::Element(el) if is_drop_revision(&el) => {
                // w:del, w:moveFrom → drop entirely
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
                new_children.extend(el.children);
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
            XMLNode::Element(el) if is_w_tag(&el, "tr") && has_row_tracking(&el, "del") => {
                // Row-level tracked deletion (w:trPr/w:del): drop the entire
                // row on accept. ECMA-376 §17.13.5.12 — a w:del inside w:trPr
                // marks the whole row as deleted; accepting a deletion yields
                // the document as if the content were never present, so the
                // row (and all its cell content) is removed. This mirrors the
                // reject path for inserted rows (w:trPr/w:ins) above, and keeps
                // this path in agreement with the IR accept path
                // (tracked_model.rs), which also removes the row.
                let count = 1 + count_nested_revisions(&el);
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
                let child_opaque = inside_opaque || is_opaque_container(&el);
                normalize_children(&mut el, stats, child_opaque);
                // A surviving cell's w:cellIns marker (§17.13.5.2) is resolved
                // on accept: the cell stays, the marker goes.
                if is_w_tag(&el, "tc") {
                    let stripped = strip_cell_tracking_markers(&mut el);
                    if inside_opaque {
                        stats.opaque_resolved += stripped;
                    } else {
                        stats.revisions_resolved += stripped;
                    }
                }
                // OOXML §17.4.37: a table must contain a non-zero number of rows.
                // After acceptance drops all deleted rows, a table can end up with
                // zero w:tr children. Remove it rather than producing invalid XML.
                if is_w_tag(&el, "tbl") && !has_any_row(&el) {
                    // Table is now empty — drop it entirely.
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

    parent.children = new_children;
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
    let kept: Vec<XMLNode> = el
        .children
        .drain(..)
        .filter(|c| {
            matches!(c, XMLNode::Element(e) if reject_restore_keeps_current_child(&parent_local, e))
        })
        .collect();
    let restored = previous.children;
    el.children = match parent_local.as_str() {
        "rPr" | "sectPr" => kept.into_iter().chain(restored).collect(),
        _ => restored.into_iter().chain(kept).collect(),
    };
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
}
