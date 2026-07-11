//! Extract redline structure from DOCX bytes containing tracked changes.
//!
//! A redline DOCX encodes changes as `<w:del>`/`<w:ins>` tracked-change
//! markup. This module extracts the meaningful content — a sequence of
//! text spans per paragraph, where each span is Normal, Deleted, or Inserted.
//!
//! Two key reconstruction invariants hold for a well-formed redline:
//! - **reject-all**: Normal + Deleted text reproduces the base document
//! - **accept-all**: Normal + Inserted text reproduces the target document

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Cursor;

use xmltree::{Element, XMLNode};
use zip::ZipArchive;

use crate::numbering::{NumberingDefinitions, NumberingState};
use crate::xml_attrs::attr_get;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const MATH_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

const DOCUMENT_RELS_PART: &str = "word/_rels/document.xml.rels";
const NUMBERING_PART: &str = "word/numbering.xml";
const HEADER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
const FOOTNOTES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
const ENDNOTES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";

// ── Types ────────────────────────────────────────────────────────────────

/// Extracted redline structure from a DOCX with tracked changes.
#[derive(Debug)]
pub struct RedlineExtract {
    /// Paragraphs from `word/document.xml` `<w:body>`.
    pub body: Vec<RedlineParagraph>,
    /// Paragraphs from story parts (headers, footers, footnotes, endnotes).
    /// Keys are DOCX ZIP paths (e.g., `word/header1.xml`).
    pub stories: BTreeMap<String, Vec<RedlineParagraph>>,
}

/// A single paragraph's redline content as a sequence of spans.
#[derive(Debug, Clone)]
pub struct RedlineParagraph {
    pub spans: Vec<RedlineSpan>,
    /// Numbering prefix for the reject view (Normal + Deleted paragraphs).
    /// Computed with a counter that skips Inserted paragraphs.
    pub reject_prefix: Option<String>,
    /// Numbering prefix for the accept view (Normal + Inserted paragraphs).
    /// Computed with a counter that skips Deleted paragraphs.
    pub accept_prefix: Option<String>,
}

/// A contiguous text span classified by its tracked-change status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedlineSpan {
    /// Unchanged text (outside `<w:del>` and `<w:ins>`).
    Normal(String),
    /// Deleted text (inside `<w:del>`).
    Deleted(String),
    /// Inserted text (inside `<w:ins>`).
    Inserted(String),
}

/// Errors from extracting redline structure.
#[derive(Debug)]
pub enum RedlineExtractError {
    /// ZIP archive could not be read.
    Zip(String),
    /// A required part is missing from the archive.
    MissingPart(String),
    /// XML parsing failed for a specific part.
    XmlParse { part: String, source: String },
}

impl fmt::Display for RedlineExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zip(msg) => write!(f, "ZIP error: {msg}"),
            Self::MissingPart(part) => write!(f, "missing required part: {part}"),
            Self::XmlParse { part, source } => write!(f, "XML parse error in {part}: {source}"),
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// Extract redline structure from raw DOCX bytes.
///
/// Fails explicitly if the DOCX is malformed or missing required parts.
pub fn extract_redline(docx_bytes: &[u8]) -> Result<RedlineExtract, RedlineExtractError> {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).map_err(|e| RedlineExtractError::Zip(e.to_string()))?;
    let numbering_defs = read_numbering_definitions(&mut zip)?;

    // Extract body from word/document.xml (required).
    let body = {
        let xml = read_zip_entry(&mut zip, "word/document.xml")?;
        let root = parse_xml("word/document.xml", &xml)?;
        let body_el = find_body(&root).ok_or_else(|| RedlineExtractError::XmlParse {
            part: "word/document.xml".to_string(),
            source: "no <w:body> element found".to_string(),
        })?;
        extract_paragraphs(body_el, numbering_defs.as_ref())
    };

    // Extract story parts referenced from document relationships.
    let mut stories = BTreeMap::new();
    for part_path in collect_referenced_story_parts(&mut zip)? {
        let xml = read_zip_entry(&mut zip, &part_path)?;
        let root = parse_xml(&part_path, &xml)?;
        let paragraphs = extract_paragraphs(&root, numbering_defs.as_ref());
        if paragraphs.is_empty() && has_element_children(&root) {
            tracing::warn!(
                part = %part_path,
                "story part exists with XML content but produced zero paragraphs"
            );
        }
        if !paragraphs.is_empty() {
            stories.insert(part_path, paragraphs);
        }
    }

    Ok(RedlineExtract { body, stories })
}

impl RedlineParagraph {
    /// Reconstruct text as if all changes accepted: Normal + Inserted.
    pub fn accept_text(&self) -> String {
        let mut out = String::new();
        if let Some(ref prefix) = self.accept_prefix {
            out.push_str(prefix);
            out.push('\t');
        }
        for span in &self.spans {
            match span {
                RedlineSpan::Normal(t) | RedlineSpan::Inserted(t) => out.push_str(t),
                RedlineSpan::Deleted(_) => {}
            }
        }
        normalize_leading_literal_prefix_separator(&out)
    }

    /// Reconstruct text as if all changes rejected: Normal + Deleted.
    pub fn reject_text(&self) -> String {
        let mut out = String::new();
        if let Some(ref prefix) = self.reject_prefix {
            out.push_str(prefix);
            out.push('\t');
        }
        for span in &self.spans {
            match span {
                RedlineSpan::Normal(t) | RedlineSpan::Deleted(t) => out.push_str(t),
                RedlineSpan::Inserted(_) => {}
            }
        }
        normalize_leading_literal_prefix_separator(&out)
    }
}

impl RedlineExtract {
    /// Collect all deleted text across body and stories.
    pub fn all_deleted_text(&self) -> Vec<String> {
        let mut result = Vec::new();
        let all_paragraphs = self.body.iter().chain(self.stories.values().flatten());
        for para in all_paragraphs {
            for span in &para.spans {
                if let RedlineSpan::Deleted(t) = span
                    && !t.is_empty()
                {
                    result.push(t.clone());
                }
            }
        }
        result
    }

    /// Collect all inserted text across body and stories.
    pub fn all_inserted_text(&self) -> Vec<String> {
        let mut result = Vec::new();
        let all_paragraphs = self.body.iter().chain(self.stories.values().flatten());
        for para in all_paragraphs {
            for span in &para.spans {
                if let RedlineSpan::Inserted(t) = span
                    && !t.is_empty()
                {
                    result.push(t.clone());
                }
            }
        }
        result
    }

    /// Collect deleted and inserted text from a specific story part.
    pub fn tracked_changes_in(&self, part: &str) -> Option<(Vec<String>, Vec<String>)> {
        let paragraphs = self.stories.get(part)?;
        let mut deleted = Vec::new();
        let mut inserted = Vec::new();
        for para in paragraphs {
            for span in &para.spans {
                match span {
                    RedlineSpan::Deleted(t) if !t.is_empty() => deleted.push(t.clone()),
                    RedlineSpan::Inserted(t) if !t.is_empty() => inserted.push(t.clone()),
                    _ => {}
                }
            }
        }
        Some((deleted, inserted))
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────

/// Check if an element is a Word namespace tag with the given local name.
///
/// Matches when the element has the correct local name AND either:
/// - the `w:` prefix (explicit namespace prefix), or
/// - the Word namespace URI (e.g., default namespace declaration).
///
/// Elements with no namespace and no `w:` prefix do NOT match — this prevents
/// bare, namespace-less elements from being incorrectly treated as Word elements.
fn is_w_tag(element: &Element, local: &str) -> bool {
    let name_local = match element.name.rsplit_once(':') {
        Some((_, l)) => l,
        None => &element.name,
    };
    if name_local != local {
        return false;
    }
    if element.prefix.as_deref() == Some("w") {
        return true;
    }
    if element.namespace.as_deref() == Some(WORD_NS) {
        return true;
    }
    // Fallback: name contains embedded prefix (e.g., "w:p" as the full name).
    // This handles parsers or constructors that don't separate prefix from name.
    element.name == format!("w:{local}")
}

/// Check if an element is a Math namespace tag with the given local name.
fn is_m_tag(element: &Element, local: &str) -> bool {
    let name_local = match element.name.rsplit_once(':') {
        Some((_, l)) => l,
        None => &element.name,
    };
    if name_local != local {
        return false;
    }
    if element.prefix.as_deref() == Some("m") {
        return true;
    }
    element.namespace.as_deref() == Some(MATH_NS)
}

/// Run-level children that represent opaque inline content.
/// These produce `\u{FFFC}` in the canonical text model.
///
/// Derives from `word_ir::RUN_WIDGET_NAMES` (the single source of truth) so it
/// cannot drift out of sync with the canonical import's widget classification.
/// Namespace discipline is preserved: `oMath` lives in the math namespace, every
/// other run widget in wordprocessingml.
fn is_opaque_run_element(el: &Element) -> bool {
    let local = match el.name.rsplit_once(':') {
        Some((_, l)) => l,
        None => el.name.as_str(),
    };
    if !crate::word_ir::is_run_widget(local) {
        return false;
    }
    if local == "oMath" {
        is_m_tag(el, "oMath")
    } else {
        is_w_tag(el, local)
    }
}

/// Elements that represent opaque content when found as direct paragraph children.
///
/// This covers two cases:
/// 1. Native paragraph-level opaque elements: `m:oMath`, `m:oMathPara`
/// 2. Run-level widgets that `build_paragraph_with_opaques()` pushes as bare
///    paragraph children during redline export (their `raw_xml` is deserialized
///    without a wrapping `<w:r>`): drawings, footnote/endnote refs, fields, etc.
/// 3. MC AlternateContent blocks wrapping opaque content (e.g., drawings).
fn is_opaque_paragraph_element(el: &Element) -> bool {
    // OMML math
    is_m_tag(el, "oMath")
        || is_m_tag(el, "oMathPara")
        // Hyperlinks are modeled as opaque inline barriers in canonical text.
        || is_w_tag(el, "hyperlink")
        // Run-level widgets that may appear bare at paragraph level after export
        || is_w_tag(el, "drawing")
        || is_w_tag(el, "object")
        || is_w_tag(el, "pict")
        || is_w_tag(el, "sym")
        || is_w_tag(el, "footnoteReference")
        || is_w_tag(el, "endnoteReference")
        // Field markers
        || is_w_tag(el, "fldChar")
        || is_w_tag(el, "fldSimple")
        || is_w_tag(el, "instrText")
        || is_w_tag(el, "delInstrText")
        // smartTag is a semantic annotation wrapper. The canonical import
        // (`is_paragraph_widget` in word_ir.rs) treats it as opaque, emitting
        // a single U+FFFC barrier. Match that projection here.
        || is_w_tag(el, "smartTag")
        // MC AlternateContent (wraps drawings, etc.)
        || is_mc_alternate_content(el)
}

const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

fn is_mc_alternate_content(element: &Element) -> bool {
    let name_local = match element.name.rsplit_once(':') {
        Some((_, l)) => l,
        None => &element.name,
    };
    if name_local != "AlternateContent" {
        return false;
    }
    element.prefix.as_deref() == Some("mc")
        || element.namespace.as_deref() == Some(MC_NS)
        || element.name == "mc:AlternateContent"
}

/// Returns true for `<w:footnote>` or `<w:endnote>` elements with
/// `w:type="separator"` or `w:type="continuationSeparator"`.
fn is_separator_note(el: &Element) -> bool {
    if !is_w_tag(el, "footnote") && !is_w_tag(el, "endnote") {
        return false;
    }
    matches!(
        attr(el, "type").or_else(|| attr(el, "w:type")),
        Some("separator" | "continuationSeparator")
    )
}

/// Returns true if the element has at least one child element (not just text/whitespace).
/// Used to distinguish genuinely empty story parts (e.g., `<w:hdr/>`) from parts
/// that contain XML content but failed to produce paragraphs.
fn has_element_children(element: &Element) -> bool {
    element
        .children
        .iter()
        .any(|child| matches!(child, XMLNode::Element(_)))
}

/// Read a ZIP entry as a UTF-8 string.
fn read_zip_entry(
    zip: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<String, RedlineExtractError> {
    use std::io::Read;
    let mut file = zip
        .by_name(name)
        .map_err(|_| RedlineExtractError::MissingPart(name.to_string()))?;
    let mut out = String::new();
    file.read_to_string(&mut out)
        .map_err(|e| RedlineExtractError::Zip(format!("read {name}: {e}")))?;
    Ok(out)
}

fn read_numbering_definitions(
    zip: &mut ZipArchive<Cursor<&[u8]>>,
) -> Result<Option<NumberingDefinitions>, RedlineExtractError> {
    use std::io::Read;

    let mut file = match zip.by_name(NUMBERING_PART) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| RedlineExtractError::Zip(format!("read {NUMBERING_PART}: {e}")))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    NumberingDefinitions::parse(&bytes)
        .map(Some)
        .map_err(|e| RedlineExtractError::XmlParse {
            part: NUMBERING_PART.to_string(),
            source: e,
        })
}

/// Parse an XML string into an Element tree.
fn parse_xml(part: &str, xml: &str) -> Result<Element, RedlineExtractError> {
    crate::word_xml::parse_document_xml(xml.as_bytes()).map_err(|e| RedlineExtractError::XmlParse {
        part: part.to_string(),
        source: format!("{e:?}"),
    })
}

/// Find the `<w:body>` element inside a document root.
fn find_body(root: &Element) -> Option<&Element> {
    for child in &root.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "body")
        {
            return Some(el);
        }
    }
    None
}

fn collect_referenced_story_parts(
    zip: &mut ZipArchive<Cursor<&[u8]>>,
) -> Result<Vec<String>, RedlineExtractError> {
    use std::io::Read;

    let mut rels_file = match zip.by_name(DOCUMENT_RELS_PART) {
        Ok(file) => file,
        Err(_) => return Ok(Vec::new()),
    };
    let mut rels_xml = String::new();
    rels_file
        .read_to_string(&mut rels_xml)
        .map_err(|e| RedlineExtractError::Zip(format!("read {DOCUMENT_RELS_PART}: {e}")))?;
    let rels_root = parse_xml(DOCUMENT_RELS_PART, &rels_xml)?;
    Ok(collect_referenced_story_parts_from_rels_root(&rels_root))
}

fn collect_referenced_story_parts_from_rels_root(rels_root: &Element) -> Vec<String> {
    let mut story_parts = BTreeSet::new();
    for child in &rels_root.children {
        let rel = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if element_local_name(rel) != "Relationship" {
            continue;
        }
        let Some(rel_type) = attr(rel, "Type") else {
            continue;
        };
        let is_story_rel = matches!(
            rel_type,
            HEADER_REL_TYPE | FOOTER_REL_TYPE | FOOTNOTES_REL_TYPE | ENDNOTES_REL_TYPE
        );
        if !is_story_rel {
            continue;
        }
        let Some(target) = attr(rel, "Target") else {
            continue;
        };
        story_parts.insert(relationship_target_to_part_path(target));
    }
    story_parts.into_iter().collect()
}

fn relationship_target_to_part_path(target: &str) -> String {
    if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("word/") {
        target.to_string()
    } else {
        format!("word/{target}")
    }
}

fn element_local_name(element: &Element) -> &str {
    match element.name.rsplit_once(':') {
        Some((_, local)) => local,
        None => &element.name,
    }
}

fn attr<'a>(element: &'a Element, key: &str) -> Option<&'a str> {
    attr_get(element, key).map(String::as_str)
}

/// Extract all paragraphs from an element, recursively descending into tables.
///
/// Numbering prefixes are computed separately for reject and accept views so
/// that Inserted paragraphs don't increment the reject counter and Deleted
/// paragraphs don't increment the accept counter.
fn extract_paragraphs(
    root: &Element,
    numbering_defs: Option<&NumberingDefinitions>,
) -> Vec<RedlineParagraph> {
    let mut elements = Vec::new();
    collect_paragraph_elements(root, &mut elements, SpanContext::Normal);

    // Phase 1: build paragraphs (spans only, no numbering) and record context.
    let mut paras: Vec<(RedlineParagraph, SpanContext, &Element)> = elements
        .iter()
        .map(|(el, row_ctx)| {
            let para_ctx = paragraph_effective_context(el, *row_ctx);
            let para = paragraph_to_redline(el, *row_ctx);
            (para, para_ctx, *el)
        })
        .collect();

    // Phase 2: reject-view numbering (skip Inserted paragraphs).
    // Use `synthesize_reject_numbering_prefix` to respect pPrChange: when a
    // paragraph's numbering was changed by a tracked change, the reject view
    // (base state) uses the previous numbering from pPrChange.
    let mut reject_state = NumberingState::new();
    for (para, para_ctx, el) in &mut paras {
        if matches!(para_ctx, SpanContext::Inserted) {
            continue;
        }
        if let Some(prefix) =
            synthesize_reject_numbering_prefix(el, numbering_defs, &mut reject_state)
            && !prefix.is_empty()
        {
            para.reject_prefix = Some(prefix);
        }
    }

    // Phase 3: accept-view numbering (skip Deleted paragraphs).
    let mut accept_state = NumberingState::new();
    for (para, para_ctx, el) in &mut paras {
        if matches!(para_ctx, SpanContext::Deleted) {
            continue;
        }
        if let Some(prefix) = synthesize_numbering_prefix(el, numbering_defs, &mut accept_state)
            && !prefix.is_empty()
        {
            para.accept_prefix = Some(prefix);
        }
    }

    paras.into_iter().map(|(para, _, _)| para).collect()
}

/// Recursively collect all `<w:p>` elements, including inside tables.
/// Propagates row-level revision context (`<w:trPr><w:del/>` or `<w:trPr><w:ins/>`)
/// so paragraphs inside deleted/inserted rows inherit the correct default context.
fn collect_paragraph_elements<'a>(
    element: &'a Element,
    out: &mut Vec<(&'a Element, SpanContext)>,
    row_ctx: SpanContext,
) {
    // Skip separator/continuationSeparator footnotes and endnotes — these are
    // Word-internal elements that carry no user-visible content.
    if is_separator_note(element) {
        return;
    }
    if is_w_tag(element, "p") {
        out.push((element, row_ctx));
        return;
    }
    // If this is a <w:tr>, check <w:trPr> for row-level revision marks.
    let ctx = if is_w_tag(element, "tr") {
        detect_row_revision_context(element).unwrap_or(row_ctx)
    } else {
        row_ctx
    };
    // Body-level SDTs are treated as opaque blocks in the canonical model and
    // excluded from text comparison. Skip them here so the redline extract
    // stays consistent — their inner paragraphs must not appear in reject/accept
    // text. SDTs inside table cells are fine; those are handled via SdtWrapper
    // and their content is included in canonical text.
    let is_body = is_w_tag(element, "body");
    for child in &element.children {
        if let XMLNode::Element(child_el) = child {
            if is_body && is_w_tag(child_el, "sdt") {
                continue;
            }
            collect_paragraph_elements(child_el, out, ctx);
        }
    }
}

/// Detect row-level revision context from `<w:trPr>`.
/// Returns `Some(SpanContext::Deleted)` if `<w:trPr><w:del .../>` is present,
/// `Some(SpanContext::Inserted)` if `<w:trPr><w:ins .../>` is present,
/// `None` otherwise.
fn detect_row_revision_context(tr: &Element) -> Option<SpanContext> {
    for child in &tr.children {
        if let XMLNode::Element(trpr) = child
            && is_w_tag(trpr, "trPr")
        {
            for prop_child in &trpr.children {
                if let XMLNode::Element(el) = prop_child {
                    if is_w_tag(el, "del") {
                        return Some(SpanContext::Deleted);
                    }
                    if is_w_tag(el, "ins") {
                        return Some(SpanContext::Inserted);
                    }
                }
            }
        }
    }
    None
}

/// Convert a `<w:p>` element into a `RedlineParagraph` with merged spans.
///
/// `row_ctx` provides the default context inherited from row-level revision marks.
/// Runs already wrapped in `<w:del>`/`<w:ins>` keep their own context — the row
/// context only applies as the default for Normal runs.
///
/// The paragraph mark status (`<w:pPr><w:rPr><w:del/>` or `<w:ins/>`) determines
/// the default context for bare inline content. There are two cases:
///
/// 1. **Wholly tracked paragraph** (no explicit `<w:del>`/`<w:ins>` containers
///    in the body): the paragraph mark status applies to ALL content. This is
///    how Word marks wholly-deleted or wholly-inserted paragraphs — the content
///    is implicitly tracked via the paragraph mark.
///
/// 2. **Mixed paragraph** (has explicit `<w:del>`/`<w:ins>` containers): the
///    paragraph mark tracks only the ¶ boundary (e.g., paragraph split). Bare
///    runs between tracked containers are Normal — they carry their own text.
fn paragraph_to_redline(paragraph: &Element, row_ctx: SpanContext) -> RedlineParagraph {
    let para_ctx = paragraph_effective_context(paragraph, row_ctx);
    let mut spans: Vec<RedlineSpan> = Vec::new();

    // Decide whether bare (unwrapped) runs inherit the paragraph mark status
    // or stay Normal.
    //
    // Wholly-tracked paragraph: no bare <w:r> at <w:p> level (all runs are
    // inside <w:del>/<w:ins> containers). The mark applies to everything.
    //
    // Mixed/annotated paragraph: bare <w:r> elements exist as direct children.
    // These are Normal text — only explicit containers carry tracking. Covers
    // split paragraphs AND Normal paragraphs whose ¶ mark was annotated by
    // annotate_paragraph_mark_status.
    let inline_ctx = if !matches!(para_ctx, SpanContext::Normal) && !has_bare_runs(paragraph) {
        para_ctx
    } else {
        row_ctx
    };
    collect_paragraph_spans(paragraph, &mut spans, inline_ctx);

    // Ensure at least one span per paragraph (empty paragraphs → Normal("")).
    if spans.is_empty() {
        let empty = match para_ctx {
            SpanContext::Normal => RedlineSpan::Normal(String::new()),
            SpanContext::Deleted => RedlineSpan::Deleted(String::new()),
            SpanContext::Inserted => RedlineSpan::Inserted(String::new()),
        };
        spans.push(empty);
    }

    RedlineParagraph {
        spans,
        reject_prefix: None,
        accept_prefix: None,
    }
}

fn paragraph_effective_context(paragraph: &Element, row_ctx: SpanContext) -> SpanContext {
    detect_paragraph_revision_context(paragraph).unwrap_or(row_ctx)
}

/// Detect paragraph-level revision context from `<w:pPr><w:rPr><w:del/>` or `<w:ins/>`.
/// Returns the default context for children in that paragraph when present.
fn detect_paragraph_revision_context(paragraph: &Element) -> Option<SpanContext> {
    for child in &paragraph.children {
        let ppr = match child {
            XMLNode::Element(el) if is_w_tag(el, "pPr") => el,
            _ => continue,
        };
        for ppr_child in &ppr.children {
            let rpr = match ppr_child {
                XMLNode::Element(el) if is_w_tag(el, "rPr") => el,
                _ => continue,
            };
            for rpr_child in &rpr.children {
                let el = match rpr_child {
                    XMLNode::Element(el) => el,
                    _ => continue,
                };
                if is_w_tag(el, "del") {
                    return Some(SpanContext::Deleted);
                }
                if is_w_tag(el, "ins") {
                    return Some(SpanContext::Inserted);
                }
            }
        }
    }
    None
}

/// Check if a paragraph has any bare `<w:r>` runs as direct children of `<w:p>`.
///
/// Bare runs only appear in the Normal-block serialization path. For block-level
/// insertions/deletions, all runs are inside `<w:ins>`/`<w:del>` containers.
///
/// Note: paragraph-level opaques (math, hyperlinks) are emitted bare in both
/// paths, so they can't distinguish wholly-tracked from annotated paragraphs.
/// Only `<w:r>` is a reliable signal.
fn has_bare_runs(paragraph: &Element) -> bool {
    for child in &paragraph.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "r")
        {
            return true;
        }
    }
    false
}

/// Context for recursive span collection — tracks whether we're inside a
/// tracked-change wrapper (`<w:del>` or `<w:ins>`).
#[derive(Clone, Copy)]
enum SpanContext {
    Normal,
    Deleted,
    Inserted,
}

/// Recursively collect spans from a paragraph element.
///
/// Handles `w:r`, `w:del`, `w:ins` directly. Skips `w:pPr` (paragraph properties).
/// For wrapper elements (e.g., `w:sdt`), recurses into children.
/// Hyperlinks are treated as opaque placeholders to match canonical projection.
/// This ensures tracked changes nested inside wrappers are not silently dropped.
///
/// Field elements (`fldChar`, `instrText`, `delInstrText`) are handled by
/// `extract_run_text` which treats them as opaque run elements producing FFFC.
/// Cached display text between separate/end passes through as normal `w:t` text,
/// except for volatile fields (DATE, TIME, etc.) where the cached result is
/// application-dependent — those emit a stable placeholder instead.
fn collect_paragraph_spans(element: &Element, spans: &mut Vec<RedlineSpan>, ctx: SpanContext) {
    collect_paragraph_spans_inner(element, spans, ctx, &mut FieldScan::None);
}

fn collect_paragraph_spans_inner(
    element: &Element,
    spans: &mut Vec<RedlineSpan>,
    ctx: SpanContext,
    field_scan: &mut FieldScan,
) {
    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        if is_w_tag(el, "pPr") {
            continue;
        } else if is_w_tag(el, "r") {
            // Runs containing field markers (fldChar, instrText) are structural
            // and always produce FFFC. Only suppress plain result-text runs.
            let has_field_marker = run_has_field_marker(el);
            update_field_scan(el, field_scan);

            if !has_field_marker && field_scan.suppress_result_text() {
                continue;
            }

            let text = extract_run_text(el);
            if !text.is_empty() {
                let span = match ctx {
                    SpanContext::Normal => RedlineSpan::Normal(text),
                    SpanContext::Deleted => RedlineSpan::Deleted(text),
                    SpanContext::Inserted => RedlineSpan::Inserted(text),
                };
                push_merged(spans, span);
            }
        } else if is_w_tag(el, "del") {
            collect_paragraph_spans_inner(el, spans, SpanContext::Deleted, field_scan);
        } else if is_w_tag(el, "ins") {
            collect_paragraph_spans_inner(el, spans, SpanContext::Inserted, field_scan);
        } else if is_w_tag(el, "fldSimple") {
            // Expand fldSimple to match the text projection of an equivalent
            // fldChar complex field: begin→FFFC, instrText→FFFC,
            // separate→FFFC, result runs→text, end→FFFC.
            // This normalizes fldSimple and fldChar to produce the same text,
            // which is required for word-parity comparisons since Word converts
            // fldSimple to fldChar during Compare Documents.
            expand_fld_simple(el, spans, ctx);
        } else if is_opaque_paragraph_element(el) {
            push_opaque_placeholder(spans, ctx);
        } else {
            // Unknown wrapper — recurse into children preserving the
            // current tracked-change context.
            collect_paragraph_spans_inner(el, spans, ctx, field_scan);
        }
    }
}

// ── Volatile field tracking ─────────────────────────────────────────────
//
// Per ECMA-376 §17.16: "As to when any field is updated is outside the
// scope of ECMA-376." DATE, TIME, and similar fields produce different
// cached results depending on when the application evaluates them.
// Word re-evaluates DATE fields during CompareDocuments; stemma preserves
// the cached result. Both are valid, but the cached text is not comparable.
//
// The field scan tracks fldChar complex field sequences and suppresses
// the cached result text for volatile fields, producing identical extracts
// regardless of when the field was evaluated.

/// Tracks state across a fldChar complex field sequence.
///
/// The sequence is: begin → instrText → separate → result runs → end.
/// These runs may span tracked-change containers (del, ins), so the
/// state is threaded through recursive calls.
enum FieldScan {
    None,
    /// Saw fldChar begin, waiting for instrText.
    AwaitingInstr,
    /// Saw instrText, know whether the field is volatile.
    InField {
        volatile: bool,
    },
    /// Past fldChar separate — result runs follow. If volatile, suppress them.
    InResult {
        volatile: bool,
    },
}

impl FieldScan {
    fn suppress_result_text(&self) -> bool {
        matches!(self, FieldScan::InResult { volatile: true })
    }
}

/// Check if a field instruction is for a volatile field whose cached result
/// is application-dependent and not suitable for text comparison.
fn is_volatile_field_instruction(instr: &str) -> bool {
    let trimmed = instr.trim();
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    matches!(
        first_word.to_uppercase().as_str(),
        "DATE" | "TIME" | "NOW" | "CREATEDATE" | "SAVEDATE" | "PRINTDATE"
    )
}

/// Returns true if the run contains fldChar or instrText — structural field
/// markers that should never be suppressed by volatile field tracking.
fn run_has_field_marker(run: &Element) -> bool {
    run.children.iter().any(|c| {
        matches!(c, XMLNode::Element(el)
            if is_w_tag(el, "fldChar") || is_w_tag(el, "instrText") || is_w_tag(el, "delInstrText"))
    })
}

/// Update the field state machine based on fldChar/instrText elements in a run.
fn update_field_scan(run: &Element, scan: &mut FieldScan) {
    for child in &run.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        if is_w_tag(el, "fldChar") {
            match attr(el, "fldCharType") {
                Some("begin") => *scan = FieldScan::AwaitingInstr,
                Some("separate") => {
                    if let FieldScan::InField { volatile } = *scan {
                        *scan = FieldScan::InResult { volatile };
                    }
                }
                Some("end") => *scan = FieldScan::None,
                _ => {}
            }
        } else if (is_w_tag(el, "instrText") || is_w_tag(el, "delInstrText"))
            && matches!(*scan, FieldScan::AwaitingInstr)
        {
            let mut instr = String::new();
            for node in &el.children {
                if let XMLNode::Text(text) = node {
                    instr.push_str(text);
                }
            }
            *scan = FieldScan::InField {
                volatile: is_volatile_field_instruction(&instr),
            };
        }
    }
}

fn push_opaque_placeholder(spans: &mut Vec<RedlineSpan>, ctx: SpanContext) {
    let placeholder = "\u{FFFC}".to_string();
    let span = match ctx {
        SpanContext::Normal => RedlineSpan::Normal(placeholder),
        SpanContext::Deleted => RedlineSpan::Deleted(placeholder),
        SpanContext::Inserted => RedlineSpan::Inserted(placeholder),
    };
    push_merged(spans, span);
}

/// Expand a `<w:fldSimple>` element to match the text projection of an
/// equivalent `fldChar` complex field.
///
/// A `fldSimple` is shorthand for a complex field:
///   fldChar begin | instrText | fldChar separate | result runs | fldChar end
///
/// Word converts fldSimple to fldChar during Compare Documents, so the
/// redline extract must produce identical text for both representations.
/// This emits: FFFC (begin) + FFFC (instrText) + FFFC (separate) + result
/// text from child runs + FFFC (end).
///
/// For volatile fields (DATE, TIME, etc.), the cached result text is
/// suppressed since it's application-dependent (§17.16: "As to when any
/// field is updated is outside the scope of ECMA-376").
fn expand_fld_simple(element: &Element, spans: &mut Vec<RedlineSpan>, ctx: SpanContext) {
    let instr = attr(element, "instr").unwrap_or_default();
    let volatile = is_volatile_field_instruction(instr);

    // Emit three FFFC markers for begin + instrText + separate
    for _ in 0..3 {
        push_opaque_placeholder(spans, ctx);
    }
    // Emit cached result text (suppressed for volatile fields)
    if !volatile {
        for child in &element.children {
            if let XMLNode::Element(el) = child
                && is_w_tag(el, "r")
            {
                let text = extract_run_text(el);
                if !text.is_empty() {
                    let span = match ctx {
                        SpanContext::Normal => RedlineSpan::Normal(text),
                        SpanContext::Deleted => RedlineSpan::Deleted(text),
                        SpanContext::Inserted => RedlineSpan::Inserted(text),
                    };
                    push_merged(spans, span);
                }
            }
        }
    }
    // Emit FFFC marker for end
    push_opaque_placeholder(spans, ctx);
}

/// Extract text content from a `<w:r>` element.
///
/// Handles `<w:t>` (normal text), `<w:delText>` (deleted text),
/// `<w:tab>` (tab character), and `<w:br>` (line break).
fn extract_run_text(run: &Element) -> String {
    let mut out = String::new();
    for child in &run.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if is_w_tag(el, "t") || is_w_tag(el, "delText") {
            for node in &el.children {
                if let XMLNode::Text(text) = node {
                    out.push_str(text);
                }
            }
        } else if is_w_tag(el, "tab") {
            out.push('\t');
        } else if is_w_tag(el, "br") {
            out.push('\n');
        } else if is_opaque_run_element(el) || is_mc_alternate_content(el) {
            out.push('\u{FFFC}');
        }
    }
    if run_has_caps(run) {
        out.to_uppercase()
    } else {
        out
    }
}

/// Returns true when the run has an explicit `<w:rPr><w:caps .../></w:rPr>`
/// property enabled.
///
/// This mirrors the text projection used by the diff/view path so redline
/// extraction compares against canonical text in the same coordinate space.
fn run_has_caps(run: &Element) -> bool {
    for child in &run.children {
        let rpr = match child {
            XMLNode::Element(el) if is_w_tag(el, "rPr") => el,
            _ => continue,
        };
        for rpr_child in &rpr.children {
            let caps = match rpr_child {
                XMLNode::Element(el) if is_w_tag(el, "caps") => el,
                _ => continue,
            };
            return match attr(caps, "val") {
                // <w:caps/> without explicit val means enabled.
                None => true,
                Some(v) => !matches!(v, "0" | "false" | "off"),
            };
        }
    }
    false
}

fn synthesize_numbering_prefix(
    paragraph: &Element,
    numbering_defs: Option<&NumberingDefinitions>,
    numbering_state: &mut NumberingState,
) -> Option<String> {
    let defs = numbering_defs?;
    let (num_id, ilvl) = extract_num_props(paragraph)?;
    match numbering_state.synthesize(defs, num_id, ilvl) {
        Ok(text) => Some(text),
        Err(e) => {
            tracing::warn!(
                "failed to synthesize numbering for numId={}, ilvl={}: {}",
                num_id,
                ilvl,
                e
            );
            None
        }
    }
}

/// Synthesize a numbering prefix for the **reject view** of a paragraph.
///
/// In the reject view we reconstruct the base document. When a paragraph has
/// `pPrChange` (tracked paragraph formatting change), the *previous* formatting
/// is the base state. If `pPrChange`'s inner pPr carries a different `numPr`,
/// we must use that previous numId/ilvl for counter synthesis — the current
/// `numPr` reflects the target state which is not part of the reject view.
///
/// Cases handled by `extract_reject_num_props`:
/// 1. No `pPrChange` → use current `numPr` (unchanged paragraph).
/// 2. `pPrChange` with same `numPr` → use current (no numbering change).
/// 3. `pPrChange` with different `numPr` → use previous numbering from pPrChange.
/// 4. `pPrChange` with no `numPr` inside → numbering was added; skip (return None).
///
/// Additionally, when numbering was REMOVED (current has no numPr, pPrChange has
/// numPr), the merge pipeline materialized the previous prefix as deleted inline
/// text. We advance the counter to keep subsequent list items correct, but do
/// NOT emit a prefix — the materialized text already provides it.
fn synthesize_reject_numbering_prefix(
    paragraph: &Element,
    numbering_defs: Option<&NumberingDefinitions>,
    numbering_state: &mut NumberingState,
) -> Option<String> {
    let defs = numbering_defs?;
    let reject_props = extract_reject_num_props(paragraph);

    // Detect the "numbering removed + materialized" case: current pPr has no
    // numPr, but pPrChange records previous numbering. The merge pipeline
    // materialized the old prefix as deleted inline text, so we must advance
    // the counter but NOT emit a prefix (it would double with the text).
    if let Some((num_id, ilvl)) = reject_props
        && extract_num_props(paragraph).is_none()
    {
        // Advance the counter (ignore the returned text).
        let _ = numbering_state.synthesize(defs, num_id, ilvl);
        return None;
    }

    let (num_id, ilvl) = reject_props?;
    match numbering_state.synthesize(defs, num_id, ilvl) {
        Ok(text) => Some(text),
        Err(e) => {
            tracing::warn!(
                "failed to synthesize reject numbering for numId={}, ilvl={}: {}",
                num_id,
                ilvl,
                e
            );
            None
        }
    }
}

fn extract_num_props(paragraph: &Element) -> Option<(u32, u32)> {
    let ppr = find_w_child(paragraph, "pPr")?;
    extract_num_props_from_ppr(ppr)
}

/// Extract numbering properties for the **reject view**.
///
/// When a paragraph has `pPrChange`, the reject view (base state) may need
/// different numbering than the current `numPr`:
///
/// 1. pPrChange has different `numPr` → use previous (reject = base state).
/// 2. pPrChange has `numId=0` → numbering was explicitly absent in the base.
///    The merge pipeline emits `numId=0` when the base had no numbering at all
///    (no numPr AND no literal prefix). Return `None` to skip in reject view.
/// 3. pPrChange has no `numPr` element → the base may have had a literal prefix
///    that was replaced by structural numbering. Use the current numPr (it
///    produces the same text as the old literal prefix).
/// 4. No pPrChange → unchanged; use current `numPr`.
fn extract_reject_num_props(paragraph: &Element) -> Option<(u32, u32)> {
    let ppr = find_w_child(paragraph, "pPr")?;
    let current = extract_num_props_from_ppr(ppr);

    // Check for pPrChange with different numbering.
    if let Some(ppr_change) = find_w_child(ppr, "pPrChange")
        && let Some(inner_ppr) = find_w_child(ppr_change, "pPr")
    {
        // Check if pPrChange explicitly has numPr (even numId=0).
        let has_numpr_element = find_w_child(inner_ppr, "numPr").is_some();
        let previous = extract_num_props_from_ppr(inner_ppr);

        match (current, previous, has_numpr_element) {
            // Both present and same → use current (no change).
            (Some(c), Some(p), _) if c == p => return current,
            // Both present but different → use previous (reject = base state).
            (Some(_), Some(p), _) => return Some(p),
            // Current has numbering, pPrChange has numId=0 → numbering was
            // explicitly absent in the base. Skip in reject view.
            (Some(_), None, true) => return None,
            // Current has numbering, pPrChange has NO numPr element at all →
            // the base may have had a literal prefix replaced by structural
            // numbering. Use the current numPr (produces same prefix text).
            (Some(_), None, false) => return current,
            // Current has no numbering, previous did → numbering was removed.
            // In the reject view (base state), this paragraph had numbering.
            (None, Some(p), _) => return Some(p),
            // Neither has numbering → no numbering in either view.
            (None, None, _) => return None,
        }
    }

    // No pPrChange → unchanged; use current numPr.
    current
}

/// Extract numId and ilvl from a pPr element (works for both main pPr and
/// the inner pPr inside pPrChange).
fn extract_num_props_from_ppr(ppr: &Element) -> Option<(u32, u32)> {
    let num_pr = find_w_child(ppr, "numPr")?;
    let num_id = find_w_child(num_pr, "numId")
        .and_then(|e| attr(e, "val"))
        .and_then(|v| v.parse::<u32>().ok())?;
    // numId=0 means "no numbering" (§17.9.18) — treat as absent.
    if num_id == 0 {
        return None;
    }
    let ilvl = find_w_child(num_pr, "ilvl")
        .and_then(|e| attr(e, "val"))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    Some((num_id, ilvl))
}

fn find_w_child<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    for child in &element.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, name)
        {
            return Some(el);
        }
    }
    None
}

/// Push a span, merging with the previous span if they have the same variant.
fn push_merged(spans: &mut Vec<RedlineSpan>, new: RedlineSpan) {
    if let Some(last) = spans.last_mut() {
        match (last, &new) {
            (RedlineSpan::Normal(existing), RedlineSpan::Normal(text)) => {
                existing.push_str(text);
                return;
            }
            (RedlineSpan::Deleted(existing), RedlineSpan::Deleted(text)) => {
                existing.push_str(text);
                return;
            }
            (RedlineSpan::Inserted(existing), RedlineSpan::Inserted(text)) => {
                existing.push_str(text);
                return;
            }
            _ => {}
        }
    }
    spans.push(new);
}

fn normalize_leading_literal_prefix_separator(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut start = 0usize;
    while start < chars.len() && chars[start].is_whitespace() {
        start += 1;
    }
    if start >= chars.len() {
        return text.to_string();
    }

    let Some(prefix_end) = detect_leading_literal_prefix_end(&chars, start) else {
        return text.to_string();
    };
    if prefix_end >= chars.len() || chars[prefix_end].is_whitespace() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + 1);
    for ch in &chars[..prefix_end] {
        out.push(*ch);
    }
    out.push(' ');
    for ch in &chars[prefix_end..] {
        out.push(*ch);
    }
    out
}

fn detect_leading_literal_prefix_end(chars: &[char], start: usize) -> Option<usize> {
    if start >= chars.len() {
        return None;
    }

    if chars[start] == '(' {
        let mut idx = start + 1;
        while idx < chars.len() && chars[idx].is_ascii_alphanumeric() {
            idx += 1;
        }
        let prefix_len = idx.saturating_sub(start + 1);
        if (1..=4).contains(&prefix_len) && idx < chars.len() && chars[idx] == ')' {
            return Some(idx + 1);
        }
        return None;
    }

    let mut idx = start;
    while idx < chars.len() && chars[idx].is_ascii_alphanumeric() {
        idx += 1;
    }
    if idx == start || idx >= chars.len() {
        return None;
    }
    let prefix_len = idx.saturating_sub(start);
    if chars[idx] == ')' && (1..=4).contains(&prefix_len) {
        return Some(idx + 1);
    }
    None
}
// ── Unit tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_merged_combines_same_variant() {
        let mut spans = Vec::new();
        push_merged(&mut spans, RedlineSpan::Normal("hello ".to_string()));
        push_merged(&mut spans, RedlineSpan::Normal("world".to_string()));
        assert_eq!(spans, vec![RedlineSpan::Normal("hello world".to_string())]);
    }

    #[test]
    fn push_merged_keeps_different_variants() {
        let mut spans = Vec::new();
        push_merged(&mut spans, RedlineSpan::Normal("a".to_string()));
        push_merged(&mut spans, RedlineSpan::Deleted("b".to_string()));
        push_merged(&mut spans, RedlineSpan::Inserted("c".to_string()));
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn accept_reject_text() {
        let para = RedlineParagraph {
            spans: vec![
                RedlineSpan::Normal("the quick ".to_string()),
                RedlineSpan::Deleted("brown".to_string()),
                RedlineSpan::Inserted("red".to_string()),
                RedlineSpan::Normal(" fox".to_string()),
            ],
            reject_prefix: None,
            accept_prefix: None,
        };
        assert_eq!(para.accept_text(), "the quick red fox");
        assert_eq!(para.reject_text(), "the quick brown fox");
    }

    #[test]
    fn accept_reject_text_inserts_separator_after_leading_literal_prefix() {
        let para = RedlineParagraph {
            spans: vec![
                RedlineSpan::Normal("\t(f)".to_string()),
                RedlineSpan::Deleted("All rights".to_string()),
                RedlineSpan::Inserted("The parties".to_string()),
            ],
            reject_prefix: None,
            accept_prefix: None,
        };

        assert_eq!(para.accept_text(), "\t(f) The parties");
        assert_eq!(para.reject_text(), "\t(f) All rights");
    }

    #[test]
    fn accept_reject_text_leaves_existing_separator_unchanged() {
        let para = RedlineParagraph {
            spans: vec![RedlineSpan::Normal("\t(f)\tAll rights".to_string())],
            reject_prefix: None,
            accept_prefix: None,
        };

        assert_eq!(para.reject_text(), "\t(f)\tAll rights");
    }

    #[test]
    fn paragraph_to_redline_reject_text_inserts_separator_after_literal_prefix_run() {
        let xml = parse_xml(
            "word/document.xml",
            r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:pPr>
                <w:tabs>
                  <w:tab w:val="left" w:pos="360"/>
                  <w:tab w:val="left" w:pos="720"/>
                </w:tabs>
              </w:pPr>
              <w:r><w:tab/><w:t>(f)</w:t></w:r>
              <w:del w:id="1" w:author="Stemma" w:date="2025-01-15T10:30:00Z">
                <w:r><w:delText>All</w:delText></w:r>
              </w:del>
              <w:ins w:id="2" w:author="Stemma" w:date="2025-01-15T10:30:00Z">
                <w:r><w:t>The parties agree that this Safe (and all the</w:t></w:r>
              </w:ins>
              <w:r><w:t xml:space="preserve"> rights and obligations hereunder </w:t></w:r>
            </w:p>"#,
        )
        .expect("valid xml");

        let para = paragraph_to_redline(&xml, SpanContext::Normal);
        assert_eq!(
            para.reject_text(),
            "\t(f) All rights and obligations hereunder "
        );
    }

    #[test]
    fn accept_reject_text_keeps_hierarchical_heading_number_tight() {
        let para = RedlineParagraph {
            spans: vec![RedlineSpan::Normal("I.1 Structure".to_string())],
            reject_prefix: None,
            accept_prefix: None,
        };

        assert_eq!(para.accept_text(), "I.1 Structure");
        assert_eq!(para.reject_text(), "I.1 Structure");
    }

    #[test]
    fn normalize_leading_literal_prefix_separator_skips_decimal_heading() {
        assert_eq!(
            normalize_leading_literal_prefix_separator("1.2 Scope"),
            "1.2 Scope"
        );
        assert_eq!(
            normalize_leading_literal_prefix_separator("I.1 Structure"),
            "I.1 Structure"
        );
        assert_eq!(
            normalize_leading_literal_prefix_separator("2.Quantitative data:"),
            "2.Quantitative data:"
        );
    }

    #[test]
    fn normalize_leading_literal_prefix_separator_keeps_bare_dotted_heading_tight() {
        assert_eq!(
            normalize_leading_literal_prefix_separator("1.Events"),
            "1.Events"
        );
    }

    #[test]
    fn normalize_leading_literal_prefix_separator_skips_regular_word_before_paren() {
        assert_eq!(
            normalize_leading_literal_prefix_separator(
                "interest). Lending stock is taken as a proxy."
            ),
            "interest). Lending stock is taken as a proxy."
        );
    }

    #[test]
    fn empty_paragraph_gets_normal_empty_span() {
        let el = Element::parse(Cursor::new(
            b"<w:p xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"></w:p>"
                as &[u8],
        ))
        .unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(para.spans, vec![RedlineSpan::Normal(String::new())]);
    }

    #[test]
    fn nested_hyperlink_normal_run() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:hyperlink>
                <w:r><w:t>link text</w:t></w:r>
            </w:hyperlink>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal("\u{FFFC}".to_string())]
        );
    }

    #[test]
    fn nested_hyperlink_tracked_changes() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:del w:author="test">
                <w:hyperlink><w:r><w:t>old link</w:t></w:r></w:hyperlink>
            </w:del>
            <w:ins w:author="test">
                <w:hyperlink><w:r><w:t>new link</w:t></w:r></w:hyperlink>
            </w:ins>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![
                RedlineSpan::Deleted("\u{FFFC}".to_string()),
                RedlineSpan::Inserted("\u{FFFC}".to_string()),
            ]
        );
    }

    #[test]
    fn extract_paragraphs_synthesizes_bullet_prefix() {
        let numbering_xml = br#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0">
                <w:lvl w:ilvl="0">
                    <w:numFmt w:val="bullet"/>
                    <w:lvlText w:val="&#x2022;"/>
                </w:lvl>
            </w:abstractNum>
            <w:num w:numId="1">
                <w:abstractNumId w:val="0"/>
            </w:num>
        </w:numbering>"#;
        let defs = NumberingDefinitions::parse(numbering_xml).expect("valid numbering xml");

        let doc =
            br#"<w:body xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:p>
                <w:pPr>
                    <w:numPr>
                        <w:ilvl w:val="0"/>
                        <w:numId w:val="1"/>
                    </w:numPr>
                </w:pPr>
                <w:r><w:t>Includes all shares.</w:t></w:r>
            </w:p>
        </w:body>"#;
        let root = Element::parse(Cursor::new(doc as &[u8])).unwrap();
        let paras = extract_paragraphs(&root, Some(&defs));
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].accept_text(), "•\tIncludes all shares.");
        assert_eq!(paras[0].reject_text(), "•\tIncludes all shares.");
    }

    #[test]
    fn nested_sdt_with_runs() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:sdt>
                <w:sdtContent>
                    <w:r><w:t>structured content</w:t></w:r>
                </w:sdtContent>
            </w:sdt>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal("structured content".to_string())]
        );
    }

    #[test]
    fn run_with_drawing_produces_placeholder() {
        let xml = br#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:drawing />
        </w:r>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        assert_eq!(extract_run_text(&el), "\u{FFFC}");
    }

    #[test]
    fn run_with_footnote_reference_produces_placeholder() {
        let xml = br#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:footnoteReference w:id="1" />
        </w:r>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        assert_eq!(extract_run_text(&el), "\u{FFFC}");
    }

    #[test]
    fn run_with_mixed_text_and_opaque() {
        let xml = br#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:t>text</w:t>
            <w:drawing />
        </w:r>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        assert_eq!(extract_run_text(&el), "text\u{FFFC}");
    }

    #[test]
    fn run_with_mc_alternate_content_produces_placeholder() {
        let xml = br#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                           xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <w:t>PRO</w:t>
            <mc:AlternateContent>
                <mc:Choice Requires="wps"><w:drawing /></mc:Choice>
            </mc:AlternateContent>
            <w:t>DUCT</w:t>
        </w:r>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        assert_eq!(extract_run_text(&el), "PRO\u{FFFC}DUCT");
    }

    #[test]
    fn run_with_caps_projects_uppercase() {
        let el = Element::parse(Cursor::new(
            br#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:rPr><w:caps/></w:rPr>
                <w:t>Company Name</w:t>
            </w:r>"# as &[u8],
        ))
        .unwrap();
        assert_eq!(extract_run_text(&el), "COMPANY NAME");
    }

    #[test]
    fn run_with_caps_off_preserves_original_case() {
        let el = Element::parse(Cursor::new(
            br#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:rPr><w:caps w:val="0"/></w:rPr>
                <w:t>Company Name</w:t>
            </w:r>"# as &[u8],
        ))
        .unwrap();
        assert_eq!(extract_run_text(&el), "Company Name");
    }

    #[test]
    fn paragraph_with_omml_in_del() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                           xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
            <w:del w:author="test">
                <m:oMath><m:r><m:t>x</m:t></m:r></m:oMath>
            </w:del>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Deleted("\u{FFFC}".to_string())]
        );
    }

    #[test]
    fn paragraph_with_omml_normal() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                           xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
            <m:oMath><m:r><m:t>x+y</m:t></m:r></m:oMath>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal("\u{FFFC}".to_string())]
        );
    }

    #[test]
    fn omath_para_with_internal_wdel_is_opaque() {
        // stemma's merge_omath produces w:del/w:ins INSIDE m:oMath (as direct
        // children of m:oMath, not inside m:r). The paragraph child is m:oMathPara.
        // redline_extract must treat the whole m:oMathPara as opaque → FFFC,
        // not recurse into the w:del elements inside.
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                           xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
            <m:oMathPara xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"
                         xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <m:oMath>
                    <w:del w:id="2" w:author="Stemma" w:date="2025-01-15T10:30:00Z">
                        <m:sSup><m:sSupPr><m:ctrlPr/></m:sSupPr>
                            <m:e><m:r><w:rPr/><w:delText>x+a</w:delText></m:r></m:e>
                            <m:sup><m:r><w:rPr/><w:delText>n</w:delText></m:r></m:sup>
                        </m:sSup>
                    </w:del>
                    <w:ins w:id="5" w:author="Stemma" w:date="2025-01-15T10:30:00Z">
                        <m:func><m:fName><m:r><m:t>sin</m:t></m:r></m:fName>
                            <m:e><m:r><m:t>x</m:t></m:r></m:e>
                        </m:func>
                    </w:ins>
                </m:oMath>
            </m:oMathPara>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal("\u{FFFC}".to_string())],
            "m:oMathPara must be treated as opaque even when it contains w:del/w:ins inside m:oMath"
        );
    }

    #[test]
    fn paragraph_mark_deleted_with_wrapped_content() {
        // Word wraps deleted content in explicit <w:del> — the paragraph mark
        // status alone does NOT propagate to bare runs.
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                           xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
            <w:pPr><w:rPr><w:del/></w:rPr></w:pPr>
            <w:del><w:r><w:delText>deleted text</w:delText></w:r></w:del>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Deleted("deleted text".to_string())]
        );
    }

    #[test]
    fn paragraph_mark_inserted_wholly_inserted_paragraph() {
        // Wholly-inserted paragraph: mark says Inserted, no explicit
        // w:del/w:ins containers → content inherits Inserted.
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                           xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
            <w:pPr><w:rPr><w:ins/></w:rPr></w:pPr>
            <m:oMath><m:r><m:t>x+y</m:t></m:r></m:oMath>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Inserted("\u{FFFC}".to_string())]
        );
    }

    #[test]
    fn paragraph_mark_inserted_with_mixed_content_stays_normal() {
        // Split/modify case: mark says Inserted, but explicit w:del/w:ins
        // containers exist → bare runs stay Normal (only containers tracked).
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr><w:rPr><w:ins/></w:rPr></w:pPr>
            <w:r><w:t>bare text</w:t></w:r>
            <w:del><w:r><w:delText>old</w:delText></w:r></w:del>
            <w:ins><w:r><w:t>new</w:t></w:r></w:ins>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        // "bare text" should be Normal, not Inserted
        assert!(
            para.spans
                .iter()
                .any(|s| matches!(s, RedlineSpan::Normal(t) if t == "bare text"))
        );
        assert!(
            para.spans
                .iter()
                .any(|s| matches!(s, RedlineSpan::Deleted(t) if t == "old"))
        );
        assert!(
            para.spans
                .iter()
                .any(|s| matches!(s, RedlineSpan::Inserted(t) if t == "new"))
        );
    }

    #[test]
    fn paragraph_mark_inserted_bare_runs_stay_normal() {
        // Normal block whose ¶ mark was annotated Inserted (e.g., by
        // annotate_paragraph_mark_status). Bare runs are old text that
        // must stay Normal so reject_text() preserves them.
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr><w:rPr><w:ins/></w:rPr></w:pPr>
            <w:r><w:t>old unchanged text</w:t></w:r>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal("old unchanged text".to_string())]
        );
    }

    #[test]
    fn paragraph_mark_deleted_empty_paragraph() {
        // Empty paragraph with deleted mark → the paragraph break itself is deleted.
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr><w:rPr><w:del/></w:rPr></w:pPr>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(para.spans, vec![RedlineSpan::Deleted(String::new())]);
    }

    #[test]
    fn paragraph_with_fldsimple_is_opaque() {
        // fldSimple is expanded to match fldChar complex field format:
        // begin→FFFC, instrText→FFFC, separate→FFFC, result text, end→FFFC
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:fldSimple w:instr=' HYPERLINK "https://example.com" '>
                <w:r><w:t>https://example.com</w:t></w:r>
            </w:fldSimple>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal(
                "\u{FFFC}\u{FFFC}\u{FFFC}https://example.com\u{FFFC}".to_string()
            )]
        );
    }

    #[test]
    fn paragraph_with_smarttag_is_opaque() {
        // smartTag wraps text runs but the canonical model treats it as opaque.
        // The redline extract must emit U+FFFC to match the canonical projection.
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:r><w:t>SUMMARY OF P</w:t></w:r>
            <w:smartTag w:uri="urn:schemas-microsoft-com:office:smarttags" w:element="PersonName">
                <w:r><w:t>RO</w:t></w:r>
            </w:smartTag>
            <w:r><w:t>DUCT</w:t></w:r>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal("SUMMARY OF P\u{FFFC}DUCT".to_string())]
        );
    }

    #[test]
    fn is_w_tag_matches_prefixed_element() {
        let el = Element::parse(Cursor::new(
            b"<w:p xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"/>"
                as &[u8],
        ))
        .unwrap();
        assert!(is_w_tag(&el, "p"));
    }

    #[test]
    fn is_w_tag_matches_default_namespace_element() {
        let el = Element::parse(Cursor::new(
            b"<p xmlns=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"/>"
                as &[u8],
        ))
        .unwrap();
        assert!(is_w_tag(&el, "p"));
    }

    #[test]
    fn is_w_tag_rejects_bare_element_without_namespace() {
        let mut el = Element::new("p");
        el.prefix = None;
        el.namespace = None;
        assert!(
            !is_w_tag(&el, "p"),
            "bare element with no namespace should not match Word tag"
        );
    }

    #[test]
    fn is_w_tag_rejects_wrong_namespace() {
        let mut el = Element::new("p");
        el.prefix = None;
        el.namespace = Some("http://example.com/not-word".to_string());
        assert!(
            !is_w_tag(&el, "p"),
            "element with wrong namespace should not match Word tag"
        );
    }

    #[test]
    fn collect_referenced_story_parts_from_relationships_only() {
        let rels_xml = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
            <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="/word/footer2.xml"/>
            <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>
            <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes" Target="endnotes.xml"/>
            <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
        </Relationships>"#;
        let root = parse_xml("word/_rels/document.xml.rels", rels_xml).unwrap();
        assert_eq!(
            collect_referenced_story_parts_from_rels_root(&root),
            vec![
                "word/endnotes.xml".to_string(),
                "word/footer2.xml".to_string(),
                "word/footnotes.xml".to_string(),
                "word/header1.xml".to_string(),
            ]
        );
    }

    #[test]
    fn numbering_counter_skips_inserted_paragraphs_in_reject_view() {
        let numbering_xml = br#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0">
                <w:lvl w:ilvl="0">
                    <w:numFmt w:val="decimal"/>
                    <w:lvlText w:val="%1."/>
                    <w:start w:val="1"/>
                </w:lvl>
            </w:abstractNum>
            <w:num w:numId="1">
                <w:abstractNumId w:val="0"/>
            </w:num>
        </w:numbering>"#;
        let defs = NumberingDefinitions::parse(numbering_xml).expect("valid numbering xml");

        let doc =
            br#"<w:body xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:p>
                <w:pPr>
                    <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
                </w:pPr>
                <w:r><w:t>First item</w:t></w:r>
            </w:p>
            <w:p>
                <w:pPr>
                    <w:rPr><w:ins/></w:rPr>
                    <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
                </w:pPr>
                <w:r><w:t>Inserted item</w:t></w:r>
            </w:p>
            <w:p>
                <w:pPr>
                    <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
                </w:pPr>
                <w:r><w:t>Third item</w:t></w:r>
            </w:p>
        </w:body>"#;
        let root = Element::parse(Cursor::new(doc as &[u8])).unwrap();
        let paras = extract_paragraphs(&root, Some(&defs));
        assert_eq!(paras.len(), 3);

        assert_eq!(paras[0].reject_text(), "1.\tFirst item");
        assert_eq!(paras[2].reject_text(), "2.\tThird item");

        assert_eq!(paras[0].accept_text(), "1.\tFirst item");
        assert_eq!(paras[1].accept_text(), "2.\tInserted item");
        assert_eq!(paras[2].accept_text(), "3.\tThird item");
    }

    #[test]
    fn numbering_counter_skips_deleted_paragraphs_in_accept_view() {
        let numbering_xml = br#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0">
                <w:lvl w:ilvl="0">
                    <w:numFmt w:val="decimal"/>
                    <w:lvlText w:val="%1."/>
                    <w:start w:val="1"/>
                </w:lvl>
            </w:abstractNum>
            <w:num w:numId="1">
                <w:abstractNumId w:val="0"/>
            </w:num>
        </w:numbering>"#;
        let defs = NumberingDefinitions::parse(numbering_xml).expect("valid numbering xml");

        let doc =
            br#"<w:body xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:p>
                <w:pPr>
                    <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
                </w:pPr>
                <w:r><w:t>First item</w:t></w:r>
            </w:p>
            <w:p>
                <w:pPr>
                    <w:rPr><w:del/></w:rPr>
                    <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
                </w:pPr>
                <w:r><w:delText>Deleted item</w:delText></w:r>
            </w:p>
            <w:p>
                <w:pPr>
                    <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
                </w:pPr>
                <w:r><w:t>Third item</w:t></w:r>
            </w:p>
        </w:body>"#;
        let root = Element::parse(Cursor::new(doc as &[u8])).unwrap();
        let paras = extract_paragraphs(&root, Some(&defs));
        assert_eq!(paras.len(), 3);

        assert_eq!(paras[0].reject_text(), "1.\tFirst item");
        assert_eq!(paras[1].reject_text(), "2.\tDeleted item");
        assert_eq!(paras[2].reject_text(), "3.\tThird item");

        assert_eq!(paras[0].accept_text(), "1.\tFirst item");
        assert_eq!(paras[2].accept_text(), "2.\tThird item");
    }

    /// fldChar DATE field: volatile field result is suppressed (§17.16:
    /// "As to when any field is updated is outside the scope of ECMA-376").
    /// Only structural markers (begin, instrText, separate, end) emit FFFC.
    #[test]
    fn fld_char_date_suppresses_volatile_result() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:r><w:t>before </w:t></w:r>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r>
            <w:r><w:instrText> DATE \@ "MMMM d, yyyy" </w:instrText></w:r>
            <w:r><w:fldChar w:fldCharType="separate"/></w:r>
            <w:r><w:t>January 15, 2025</w:t></w:r>
            <w:r><w:fldChar w:fldCharType="end"/></w:r>
            <w:r><w:t> after</w:t></w:r>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        // DATE is volatile: begin→FFFC, instrText→FFFC, separate→FFFC,
        // cached result SUPPRESSED, end→FFFC
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal(
                "before \u{FFFC}\u{FFFC}\u{FFFC}\u{FFFC} after".to_string()
            )]
        );
    }

    /// fldSimple DATE field: volatile result suppressed, matching fldChar behavior.
    #[test]
    fn fld_simple_date_suppresses_volatile_result() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:r><w:t>before </w:t></w:r>
            <w:fldSimple w:instr=" DATE \@ &quot;MMMM d, yyyy&quot; ">
                <w:r><w:t>January 15, 2025</w:t></w:r>
            </w:fldSimple>
            <w:r><w:t> after</w:t></w:r>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal(
                "before \u{FFFC}\u{FFFC}\u{FFFC}\u{FFFC} after".to_string()
            )]
        );
    }

    /// fldChar hyperlink triplet: each opaque element produces FFFC,
    /// display text between separate/end passes through.
    #[test]
    fn fld_char_hyperlink_preserves_display_text() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:r><w:t>see </w:t></w:r>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r>
            <w:r><w:instrText>HYPERLINK "http://example.com"</w:instrText></w:r>
            <w:r><w:fldChar w:fldCharType="separate"/></w:r>
            <w:r><w:t>http://example.com</w:t></w:r>
            <w:r><w:fldChar w:fldCharType="end"/></w:r>
            <w:r><w:t> for details</w:t></w:r>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        // fldChar begin → FFFC, instrText → FFFC, fldChar separate → FFFC,
        // display text passes through, fldChar end → FFFC
        assert_eq!(
            para.spans,
            vec![RedlineSpan::Normal(
                "see \u{FFFC}\u{FFFC}\u{FFFC}http://example.com\u{FFFC} for details".to_string()
            )]
        );
    }

    /// fldChar inside tracked-change wrappers preserves the correct span context.
    #[test]
    fn fld_char_inside_tracked_change() {
        let xml = br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:r><w:t>before </w:t></w:r>
            <w:ins w:id="1" w:author="test">
                <w:r><w:fldChar w:fldCharType="begin"/></w:r>
                <w:r><w:instrText>HYPERLINK "http://example.com"</w:instrText></w:r>
                <w:r><w:fldChar w:fldCharType="separate"/></w:r>
                <w:r><w:t>link</w:t></w:r>
                <w:r><w:fldChar w:fldCharType="end"/></w:r>
            </w:ins>
            <w:r><w:t> after</w:t></w:r>
        </w:p>"#;
        let el = Element::parse(Cursor::new(xml as &[u8])).unwrap();
        let para = paragraph_to_redline(&el, SpanContext::Normal);
        // Each fldChar/instrText → FFFC (Inserted), display text passes through
        // (Inserted), text before/after is Normal.
        assert_eq!(
            para.accept_text(),
            "before \u{FFFC}\u{FFFC}\u{FFFC}link\u{FFFC} after"
        );
        assert_eq!(para.reject_text(), "before  after");
    }

    #[test]
    fn separator_notes_excluded_from_extraction() {
        let xml = r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:footnote w:type="separator" w:id="-1">
                <w:p><w:r><w:separator/></w:r></w:p>
            </w:footnote>
            <w:footnote w:type="continuationSeparator" w:id="0">
                <w:p><w:r><w:continuationSeparator/></w:r></w:p>
            </w:footnote>
            <w:footnote w:id="1">
                <w:p><w:r><w:t>Real footnote text</w:t></w:r></w:p>
            </w:footnote>
        </w:footnotes>"#;
        let root = parse_xml("word/footnotes.xml", xml).unwrap();
        let paras = extract_paragraphs(&root, None);
        // Only the real footnote (id=1) should produce a paragraph.
        // Separator and continuationSeparator footnotes must be excluded.
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].accept_text(), "Real footnote text");
    }
}
