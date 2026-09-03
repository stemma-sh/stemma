//! Local before/after compilation for explicit native edits.
//!
//! This module resolves changes inside an edit region that a caller has
//! already selected. It does not align independently authored documents or
//! infer document-level correspondence; those responsibilities belong to the
//! downstream comparison crate.

use std::collections::HashMap;
use std::env;

use sha2::{Digest, Sha256};
use similar::{Algorithm, ChangeTag, TextDiff};

// =============================================================================
// Custom tokenizer for word-level diffing
// =============================================================================

/// Character class for tokenization.
/// Each contiguous run of the same class becomes one token.
#[derive(PartialEq)]
enum CharClass {
    Word,
    Whitespace,
    Punctuation,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Whitespace
    } else {
        CharClass::Punctuation
    }
}

/// Length of the truncated hash appended to opaque `\u{FFFC}` tags.
/// Must match the truncation length in `opaque_diff_tag()`.
const OPAQUE_HASH_LEN: usize = 12;

/// Tokenize text into slices, splitting on word/whitespace/punctuation boundaries.
///
/// - Word characters (alphanumeric + underscore) are grouped into contiguous runs.
/// - Whitespace characters are grouped into contiguous runs.
/// - Each punctuation/symbol character is its own token (they are semantically independent).
///
/// Example: `"Stock);"` → `["Stock", ")", ";"]`
/// Example: `"Section 3.1(a)"` → `["Section", " ", "3", ".", "1", "(", "a", ")"]`
pub fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();

    while let Some(&(start, c)) = chars.peek() {
        let class = char_class(c);
        chars.next();

        if c == '\n' {
            // Preserve hard breaks as independent physical run carriers.
            let end = chars.peek().map_or(text.len(), |&(i, _)| i);
            tokens.push(&text[start..end]);
        } else if c == '\u{FFFC}' {
            // Tagged opaque placeholders in the resolving-opaques path are
            // `FFFC + 12 hex chars`. Plain placeholders in the regular diff path
            // are just bare `FFFC`. Only consume a trailing tag when there is an
            // actual full 12-char hex suffix; otherwise leave adjacent text as its
            // own token so it keeps its own formatting.
            let mut lookahead = chars.clone();
            let mut tag_len = 0usize;
            while tag_len < OPAQUE_HASH_LEN {
                match lookahead.peek() {
                    Some(&(_, next_c)) if next_c.is_ascii_hexdigit() => {
                        lookahead.next();
                        tag_len += 1;
                    }
                    _ => break,
                }
            }
            if tag_len == OPAQUE_HASH_LEN {
                for _ in 0..OPAQUE_HASH_LEN {
                    chars.next();
                }
            }
            let end = chars.peek().map_or(text.len(), |&(i, _)| i);
            tokens.push(&text[start..end]);
        } else if class == CharClass::Punctuation {
            // Each punctuation char is its own token
            let end = chars.peek().map_or(text.len(), |&(i, _)| i);
            tokens.push(&text[start..end]);
        } else {
            // Word and whitespace: consume contiguous run of the same class
            while let Some(&(_, next_c)) = chars.peek() {
                if next_c != '\n' && char_class(next_c) == class {
                    chars.next();
                } else {
                    break;
                }
            }
            let end = chars.peek().map_or(text.len(), |&(i, _)| i);
            tokens.push(&text[start..end]);
        }
    }

    let fused_enum = fuse_legal_enumerators(tokens, text);
    fuse_intraword_apostrophes(fused_enum, text)
}

/// Check if a string is a legal enumerator content (inside parentheses).
/// Matches: single letters a-z/A-Z, roman numerals i-xiv, double letters aa-zz.
fn is_enumerator_content(s: &str) -> bool {
    // Single letter
    if s.len() == 1 {
        let c = s.as_bytes()[0];
        return c.is_ascii_alphabetic();
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
        "i" | "ii"
            | "iii"
            | "iv"
            | "v"
            | "vi"
            | "vii"
            | "viii"
            | "ix"
            | "x"
            | "xi"
            | "xii"
            | "xiii"
            | "xiv"
    )
}

/// Post-tokenization pass: fuse `(` + enumerator + `)` into single tokens.
///
/// Boundary guards prevent false fusing:
/// - Left: token before `(` must NOT end with an alphanumeric char (prevents `13(d)`)
/// - Right: token after `)` must NOT start with an alphanumeric char
fn fuse_legal_enumerators<'a>(tokens: Vec<&'a str>, text: &'a str) -> Vec<&'a str> {
    if tokens.len() < 3 {
        return tokens;
    }

    let mut result = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        if i + 2 < tokens.len() && tokens[i] == "(" && tokens[i + 2] == ")" {
            let content = tokens[i + 1];
            if is_enumerator_content(content) {
                // Left boundary: token before `(` must not end with alphanumeric
                let left_ok = if i == 0 {
                    true
                } else {
                    !tokens[i - 1]
                        .chars()
                        .last()
                        .is_some_and(|c| c.is_alphanumeric())
                };
                // Right boundary: token after `)` must not start with alphanumeric
                let right_ok = if i + 3 >= tokens.len() {
                    true
                } else {
                    !tokens[i + 3]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphanumeric())
                };

                if left_ok && right_ok {
                    // Fuse: compute the byte range spanning tokens[i] through tokens[i+2]
                    let start_ptr = tokens[i].as_ptr() as usize;
                    let end_token = tokens[i + 2];
                    let end_ptr = end_token.as_ptr() as usize + end_token.len();
                    let text_start = text.as_ptr() as usize;
                    let byte_start = start_ptr - text_start;
                    let byte_end = end_ptr - text_start;
                    result.push(&text[byte_start..byte_end]);
                    i += 3;
                    continue;
                }
            }
        }
        result.push(tokens[i]);
        i += 1;
    }

    result
}

/// Check if a character is an apostrophe (ASCII or Unicode smart quote).
fn is_apostrophe(c: char) -> bool {
    c == '\'' || c == '\u{2019}' // ASCII apostrophe or right single quotation mark
}

/// Check if a token consists entirely of word characters (alphanumeric or underscore).
fn is_word_token(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Post-tokenization pass: fuse `word + apostrophe + word` into single tokens.
///
/// Contractions and possessives like `don't`, `it's`, `refer's` should be
/// atomic tokens so the diff treats them as single units.
fn fuse_intraword_apostrophes<'a>(tokens: Vec<&'a str>, text: &'a str) -> Vec<&'a str> {
    if tokens.len() < 3 {
        return tokens;
    }

    let mut result = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        if i + 2 < tokens.len()
            && !tokens[i + 1].is_empty()
            && tokens[i + 1].chars().count() == 1
            && tokens[i + 1].chars().next().is_some_and(is_apostrophe)
            && is_word_token(tokens[i])
            && is_word_token(tokens[i + 2])
        {
            // Fuse: compute the byte range spanning tokens[i] through tokens[i+2]
            let start_ptr = tokens[i].as_ptr() as usize;
            let end_token = tokens[i + 2];
            let end_ptr = end_token.as_ptr() as usize + end_token.len();
            let text_start = text.as_ptr() as usize;
            let byte_start = start_ptr - text_start;
            let byte_end = end_ptr - text_start;
            result.push(&text[byte_start..byte_end]);
            i += 3;
        } else {
            result.push(tokens[i]);
            i += 1;
        }
    }

    result
}

use crate::domain::{
    BlockNode, CellParagraphChange, FormattingChange, InlineChange, InlineChangeSegmentType,
    InlineNode, Mark, MarkValue, NestedTableDiff, NestedTableDiffKind, OpaqueInlineNode,
    OpaqueKind, OpaqueSegmentKind, RunRprAuthored, StyleProps, TableCellChange, TableCellDiff,
    TableCellDiffType, TableDiffResult, TableNode, TableRowAlignment,
};
use crate::table_edit::{CellDiffType, RowAlignment, diff_tables};

/// Table with structure and text fingerprints for diffing.
#[derive(Clone, Debug)]
struct DiffableTable {
    table: TableNode,
}

struct OpaqueTracker<'a> {
    inline_index: usize,
    node: &'a OpaqueInlineNode,
}
pub fn extract_table_text(table: &TableNode) -> String {
    let mut out = String::new();
    if let Ok(canonical) = crate::table::canonicalize_table(table) {
        for cell in &canonical.cells {
            append_table_blocks_text(&mut out, &cell.blocks);
        }
    } else {
        // Internally constructed diagnostic values can bypass import's table
        // validator. Preserve the previous deterministic flat projection for
        // those values instead of hiding all text behind canonicalization.
        for row in &table.rows {
            for cell in &row.cells {
                append_table_blocks_text(&mut out, &cell.blocks);
            }
        }
    }
    out
}

fn append_table_blocks_text(out: &mut String, blocks: &[BlockNode]) {
    for block in blocks {
        let text = match block {
            BlockNode::Paragraph(paragraph) => {
                let inlines = paragraph.all_inlines_owned();
                let mut text = extract_inline_text(&inlines);
                if text.trim().is_empty()
                    && let Some(rendered) = &paragraph.rendered_text
                    && !rendered.trim().is_empty()
                {
                    text = rendered.clone();
                }
                text
            }
            BlockNode::Table(nested) => extract_table_text(nested),
            BlockNode::OpaqueBlock(_) => continue,
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&text);
    }
}

/// Build a TableDiffResult for a single table (no counterpart).
/// Used for inserted/deleted tables so the frontend can still render the table structure.
pub(crate) fn compute_table_diff_result(
    old_table: &TableNode,
    new_table: &TableNode,
) -> Result<TableDiffResult, String> {
    let diff = diff_tables(old_table, new_table)?;

    // Convert table_diff types to domain types
    let row_alignment: Vec<TableRowAlignment> = diff
        .row_alignment
        .into_iter()
        .map(|a| match a {
            RowAlignment::Matched { old_row, new_row } => {
                TableRowAlignment::Matched { old_row, new_row }
            }
            RowAlignment::Deleted { old_row } => TableRowAlignment::Deleted { old_row },
            RowAlignment::Inserted { new_row } => TableRowAlignment::Inserted { new_row },
        })
        .collect();

    let cell_diffs: Vec<TableCellDiff> = diff
        .cell_diffs
        .into_iter()
        .map(|d| TableCellDiff {
            old_cell_idx: d.old_cell_idx,
            new_cell_idx: d.new_cell_idx,
            diff_type: match d.diff_type {
                CellDiffType::Unchanged => TableCellDiffType::Unchanged,
                CellDiffType::Modified => TableCellDiffType::Modified,
                CellDiffType::Inserted => TableCellDiffType::Inserted,
                CellDiffType::Deleted => TableCellDiffType::Deleted,
                CellDiffType::MergeChanged => TableCellDiffType::MergeChanged,
            },
            text_diff: d.text_diff,
            nested_table_diffs: d.nested_table_diffs,
        })
        .collect();

    Ok(TableDiffResult {
        old_table: diff.old_table,
        new_table: diff.new_table,
        row_alignment,
        cell_diffs,
    })
}
pub(crate) fn find_blip_rid(xml: &str) -> Option<String> {
    // Try DrawingML r:embed first (a:blip r:embed="rIdN")
    if let Some(rid) = find_rid_by_patterns(
        xml,
        &[
            "r:embed=\"",
            " embed=\"",
            ">embed=\"",
            "\tembed=\"",
            "\nembed=\"",
        ],
    ) {
        return Some(rid);
    }

    // Try VML imagedata r:id (v:imagedata r:id="rIdN")
    find_vml_imagedata_rid(xml)
}

/// Search for a relationship ID matching any of the given attribute patterns.
fn find_rid_by_patterns(xml: &str, patterns: &[&str]) -> Option<String> {
    for pattern in patterns {
        let mut search_start = 0;
        while let Some(pos) = xml[search_start..].find(pattern) {
            let abs_pos = search_start + pos;
            let start = abs_pos + pattern.len();
            if let Some(end_offset) = xml[start..].find('"') {
                let value = &xml[start..start + end_offset];
                if value.starts_with("rId") || value.starts_with("rid") {
                    return Some(value.to_string());
                }
            }
            search_start = abs_pos + 1;
        }
    }
    None
}

/// Extract image relationship ID from VML `<v:imagedata r:id="rIdN"/>`.
///
/// VML shapes use `r:id` on `v:imagedata` elements instead of the DrawingML
/// `r:embed` on `a:blip`. Both reference the same image relationships.
fn find_vml_imagedata_rid(xml: &str) -> Option<String> {
    // Find <v:imagedata or <imagedata elements, then extract r:id
    let imagedata_markers = ["<v:imagedata", "<imagedata"];
    for marker in &imagedata_markers {
        let mut search_start = 0;
        while let Some(pos) = xml[search_start..].find(marker) {
            let abs_pos = search_start + pos;
            // Find the end of this element (> or />)
            let element_end = xml[abs_pos..]
                .find('>')
                .map(|e| abs_pos + e)
                .unwrap_or(xml.len());
            let element_text = &xml[abs_pos..element_end];

            // Look for r:id="..." within this element
            if let Some(rid) = find_rid_by_patterns(element_text, &["r:id=\"", " o:relid=\""]) {
                return Some(rid);
            }
            search_start = abs_pos + 1;
        }
    }
    None
}
/// Metadata extracted from a Drawing XML fragment for comparison.
#[derive(Debug, PartialEq, Eq)]
struct DrawingMetadata {
    extent_cx: Option<String>,
    extent_cy: Option<String>,
    src_rect: Option<String>,
    alt_text: Option<String>,
    drawing_type: DrawingType,
}

/// Classification of drawing types for generating fallback labels.
#[derive(Debug, PartialEq, Eq)]
enum DrawingType {
    RasterImage,      // Has r:embed (DrawingML) or v:imagedata r:id (VML)
    VmlShape(String), // VML shape without raster backing (rect, oval, etc.)
    WpShape(String),  // wps:wsp with preset geometry name
    Chart,            // Chart/diagram
    Unknown,
}

/// Parse drawing metadata from raw XML bytes using simple string search.
fn parse_drawing_metadata(raw_xml: &[u8]) -> DrawingMetadata {
    let xml = match std::str::from_utf8(raw_xml) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Invalid UTF-8 in drawing raw XML ({} bytes): {}",
                raw_xml.len(),
                e
            );
            return DrawingMetadata {
                extent_cx: None,
                extent_cy: None,
                src_rect: None,
                alt_text: None,
                drawing_type: DrawingType::Unknown,
            };
        }
    };

    let extent_cx = find_xml_attr(xml, "extent", "cx");
    let extent_cy = find_xml_attr(xml, "extent", "cy");

    // srcRect attributes: l, t, r, b (left, top, right, bottom crop percentages)
    let src_rect = find_element_attrs(xml, "srcRect");

    // Alt text from docPr descr attribute
    let alt_text = find_xml_attr(xml, "docPr", "descr");

    // Classify drawing type for fallback text generation
    // Use find_blip_rid to check for valid r:embed attribute (not just string presence)
    let drawing_type = if find_blip_rid(xml).is_some() {
        DrawingType::RasterImage
    } else if let Some(shape) = extract_vml_shape_type(xml) {
        DrawingType::VmlShape(shape)
    } else if let Some(preset) = extract_wp_shape_preset(xml) {
        DrawingType::WpShape(preset)
    } else if xml.contains("<c:chart") || xml.contains(":chart") {
        DrawingType::Chart
    } else {
        DrawingType::Unknown
    };

    DrawingMetadata {
        extent_cx,
        extent_cy,
        src_rect,
        alt_text,
        drawing_type,
    }
}

/// Find a specific attribute value on an element by local name.
/// Handles namespace-prefixed tags (e.g. `wp:extent` matches `extent`).
fn find_xml_attr(xml: &str, element_local_name: &str, attr_name: &str) -> Option<String> {
    // Look for the element — may be prefixed (e.g. wp:extent, a:srcRect)
    // Search for `:element_local_name ` or `<element_local_name `
    let patterns = [
        format!(":{element_local_name}"),
        format!("<{element_local_name}"),
    ];
    for pattern in &patterns {
        if let Some(elem_pos) = xml.find(pattern.as_str()) {
            // Find the end of this element's opening tag
            let rest = &xml[elem_pos..];
            let tag_end = rest.find('>').unwrap_or(rest.len());
            let tag = &rest[..tag_end];
            // Now find the attribute within this tag
            let attr_marker = format!("{attr_name}=\"");
            if let Some(attr_pos) = tag.find(&attr_marker) {
                let val_start = attr_pos + attr_marker.len();
                if let Some(val_end) = tag[val_start..].find('"') {
                    return Some(tag[val_start..val_start + val_end].to_string());
                }
            }
        }
    }
    None
}

/// Extract all attributes of an element as a sorted string for comparison.
/// Returns None if the element is not found.
fn find_element_attrs(xml: &str, element_local_name: &str) -> Option<String> {
    let patterns = [
        format!(":{element_local_name}"),
        format!("<{element_local_name}"),
    ];
    for pattern in &patterns {
        if let Some(elem_pos) = xml.find(pattern.as_str()) {
            let rest = &xml[elem_pos..];
            let tag_end = rest.find('>').unwrap_or(rest.len());
            let tag = &rest[..tag_end];
            // Extract all attr="value" pairs, sort them, join
            let mut attrs: Vec<&str> = Vec::new();
            let mut pos = 0;
            while pos < tag.len() {
                if let Some(eq_pos) = tag[pos..].find("=\"") {
                    // Walk back to find attr name start
                    let abs_eq = pos + eq_pos;
                    let name_start = tag[..abs_eq]
                        .rfind(|c: char| c.is_whitespace() || c == ':')
                        .map(|p| p + 1)
                        .unwrap_or(0);
                    let val_start = abs_eq + 2;
                    if let Some(val_end) = tag[val_start..].find('"') {
                        let abs_val_end = val_start + val_end;
                        attrs.push(&tag[name_start..abs_val_end + 1]);
                        pos = abs_val_end + 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            attrs.sort();
            return Some(attrs.join(" "));
        }
    }
    None
}

/// Extract VML shape type from pict elements.
/// Examples: <v:rect>, <v:oval>, <v:roundrect>
fn extract_vml_shape_type(xml: &str) -> Option<String> {
    let vml_shapes = [
        "<v:rect",
        "<v:oval",
        "<v:roundrect",
        "<v:shape",
        "<v:line",
        "<v:polyline",
        "<v:curve",
        "<v:arc",
    ];
    for shape in &vml_shapes {
        if xml.contains(shape) {
            // Strip "<v:" prefix to get shape name
            return Some(shape[3..].to_string());
        }
    }
    None
}

/// Extract WordprocessingML shape preset from wps:wsp elements.
/// Example: <a:prstGeom prst="rect">
fn extract_wp_shape_preset(xml: &str) -> Option<String> {
    find_xml_attr(xml, "prstGeom", "prst")
}

/// Generate descriptive fallback text for drawings without asset_ref.
/// Priority: alt text > shape type > generic label
fn extract_drawing_fallback_text(raw_xml: &[u8]) -> Option<String> {
    let meta = parse_drawing_metadata(raw_xml);

    // Priority 1: Use author-provided alt text
    if let Some(alt) = meta.alt_text {
        let trimmed = alt.trim();
        if !trimmed.is_empty() {
            return Some(format!("[drawing: {trimmed}]"));
        }
    }

    // Priority 2: Describe shape type
    match meta.drawing_type {
        DrawingType::RasterImage => None, // Should have asset_ref
        DrawingType::VmlShape(shape) => Some(format!("[shape: {shape}]")),
        DrawingType::WpShape(preset) => Some(format!("[shape: {preset}]")),
        DrawingType::Chart => Some("[chart]".to_string()),
        DrawingType::Unknown => Some("[drawing]".to_string()),
    }
}
fn opaque_placeholder(_opaque: &OpaqueInlineNode) -> String {
    "\u{FFFC}".to_string()
}

/// Tagged placeholder for opaque inlines in the inline diff path.
/// Embeds a truncated semantic-identity hash so the diff algorithm can
/// distinguish different opaques (e.g., footnote ref 1 vs footnote ref 2).
fn opaque_diff_tag(opaque: &OpaqueInlineNode) -> String {
    let identity_hash = opaque_identity_hash(opaque);
    let hash_str = identity_hash[..identity_hash.len().min(OPAQUE_HASH_LEN)].to_string();
    format!("\u{FFFC}{hash_str}")
}

fn opaque_identity_hash(opaque: &OpaqueInlineNode) -> String {
    opaque.content_hash.clone().unwrap_or_else(|| {
        // Hyperlinks (and any future kinds) without content_hash: hash the
        // semantic identity of the kind, excluding transport details like r:id
        // that differ between documents.
        sha256_hex(opaque_semantic_identity(&opaque.kind).as_bytes())
    })
}

/// Compute a stable identity string for an opaque kind, excluding fields
/// that are transport/serialization details (like r:id) rather than semantic
/// identity.
fn opaque_semantic_identity(kind: &OpaqueKind) -> String {
    match kind {
        OpaqueKind::Hyperlink(data) => {
            format!(
                "Hyperlink({:?},{:?},{:?})",
                data.url, data.anchor, data.text
            )
        }
        // All other kinds: use Debug repr (they don't have transport-only fields).
        other => format!("{other:?}"),
    }
}

fn change_type_to_segment_type(change_type: &str) -> InlineChangeSegmentType {
    match change_type {
        "insert" => InlineChangeSegmentType::Insert,
        "delete" => InlineChangeSegmentType::Delete,
        _ => InlineChangeSegmentType::Equal,
    }
}

/// The hyperlink target for the render projection: the external URL, or
/// `#anchor` for an internal bookmark link. None for non-hyperlink opaques.
pub(crate) fn opaque_url(kind: &OpaqueKind) -> Option<String> {
    match kind {
        OpaqueKind::Hyperlink(data) => data
            .url
            .clone()
            .or_else(|| data.anchor.as_ref().map(|a| format!("#{a}"))),
        _ => None,
    }
}

pub(crate) fn opaque_kind_to_segment_kind(kind: &OpaqueKind) -> OpaqueSegmentKind {
    match kind {
        OpaqueKind::Drawing => OpaqueSegmentKind::Drawing,
        // Defensive label only: compare/diff refuses quarantined inputs at
        // entry, and the quarantined kind exists only on body-level opaque
        // blocks, never inline.
        OpaqueKind::QuarantinedNestedTracking => {
            OpaqueSegmentKind::Unknown("quarantined_nested_tracked_changes".to_string())
        }
        OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline => OpaqueSegmentKind::Omml,
        OpaqueKind::Hyperlink(_) => OpaqueSegmentKind::Hyperlink,
        OpaqueKind::Field(_) => OpaqueSegmentKind::Field,
        OpaqueKind::Sdt => OpaqueSegmentKind::Sdt,
        OpaqueKind::Ruby => OpaqueSegmentKind::Ruby,
        OpaqueKind::SmartArt => OpaqueSegmentKind::SmartArt,
        OpaqueKind::CommentReference(_) => OpaqueSegmentKind::CommentReference,
        OpaqueKind::FootnoteReference(_) => OpaqueSegmentKind::FootnoteReference,
        OpaqueKind::EndnoteReference(_) => OpaqueSegmentKind::EndnoteReference,
        OpaqueKind::SmartTag => OpaqueSegmentKind::SmartTag,
        OpaqueKind::Sym(_) => OpaqueSegmentKind::Sym,
        OpaqueKind::Ptab => OpaqueSegmentKind::Ptab,
        OpaqueKind::CustomXml => OpaqueSegmentKind::CustomXml,
        OpaqueKind::Unknown(name) => OpaqueSegmentKind::Unknown(name.clone()),
    }
}

fn extract_inline_text(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => {
                // Apply caps mark to normalize text for comparison.
                // This ensures "this instrument" with Caps mark compares equal
                // to "THIS INSTRUMENT" stored as actual uppercase.
                let text = if t.style_props.caps == MarkValue::On {
                    t.text.to_uppercase()
                } else {
                    t.text.clone()
                };
                out.push_str(&text);
            }
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(o) => out.push_str(&opaque_placeholder(o)),
            InlineNode::Decoration(_) => {} // Zero-width, no contribution
            InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {} // Zero-width, no contribution
        }
    }
    out
}
/// Content significance factor for scaling match bonuses in the DP.
///
/// The DP alignment can be distorted by "structural noise": empty paragraphs and
/// trivial elements (drawings, single characters) that all share the same hash.
/// When many of these appear between anchors, the DP may prefer to match them
/// (earning many small bonuses) instead of matching content-bearing paragraphs
/// (earning fewer but more important bonuses).
///
/// This function returns a scaling factor based on non-whitespace character count:
/// - 0 chars (empty): 0.0 — matching is meaningless, neutral cost
/// - 1–5 chars (trivial): 0.1 — tiny bonus, won't distort alignment
/// - 6+ chars (content): 1.0+ — full weight, scaled by ln(len) for longer content
///
/// The logarithmic scaling for longer content ensures that matching a 1400-char
/// paragraph provides enough bonus to pull the DP through an expensive gap path,
/// rather than letting the DP take a cheaper path that deletes+reinserts it.
/// Minimum non-whitespace characters for an Unchanged span to be a "strong anchor".
const ANCHOR_MIN_CHARS: usize = 8;

/// Minimum alternating del/ins run count to trigger zipper collapse.
const ZIPPER_MIN_CHANGE_RUNS: usize = 4;

/// Regions with similarity below this are force-collapsed even without high alternation.
const LOW_SIMILARITY_THRESHOLD: f64 = 0.15;

/// If overall text similarity is below this, bail out of token-level diff entirely.
const BAIL_OUT_SIMILARITY_THRESHOLD: f64 = 0.30;

/// Minimum text length (in chars) to consider bail-out.
const BAIL_OUT_MIN_CHARS: usize = 50;

#[derive(Clone, Copy, Debug)]
struct DiffHeuristics {
    anchor_min_chars: usize,
    zipper_min_change_runs: usize,
    low_similarity_threshold: f64,
    bail_out_similarity_threshold: f64,
    bail_out_min_chars: usize,
}

impl DiffHeuristics {
    fn from_env() -> Self {
        Self {
            anchor_min_chars: parse_env_usize("DIFF_ANCHOR_MIN_CHARS").unwrap_or(ANCHOR_MIN_CHARS),
            zipper_min_change_runs: parse_env_usize("DIFF_ZIPPER_MIN_CHANGE_RUNS")
                .unwrap_or(ZIPPER_MIN_CHANGE_RUNS),
            low_similarity_threshold: parse_env_f64("DIFF_LOW_SIMILARITY_THRESHOLD")
                .unwrap_or(LOW_SIMILARITY_THRESHOLD),
            bail_out_similarity_threshold: parse_env_f64("DIFF_BAIL_OUT_SIMILARITY_THRESHOLD")
                .unwrap_or(BAIL_OUT_SIMILARITY_THRESHOLD),
            bail_out_min_chars: parse_env_usize("DIFF_BAIL_OUT_MIN_CHARS")
                .unwrap_or(BAIL_OUT_MIN_CHARS),
        }
    }
}

fn parse_env_usize(key: &str) -> Option<usize> {
    let value = env::var(key).ok()?;
    match value.parse::<usize>() {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            eprintln!("warning: {key} must be a usize, got '{value}': {err}; using default");
            None
        }
    }
}

fn parse_env_f64(key: &str) -> Option<f64> {
    let value = env::var(key).ok()?;
    match value.parse::<f64>() {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            eprintln!("warning: {key} must be an f64, got '{value}': {err}; using default");
            None
        }
    }
}
fn normalize_for_similarity(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Compute similarity between two text strings (0.0 to 1.0).
/// Uses word-level LCS ratio on normalized text.
fn text_similarity(text1: &str, text2: &str) -> f64 {
    let norm1 = normalize_for_similarity(text1);
    let norm2 = normalize_for_similarity(text2);
    let old_tokens = tokenize(&norm1);
    let new_tokens = tokenize(&norm2);
    TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens)
        .ratio() as f64
}

fn cell_block_lists_align(base: &DiffableTable, target: &DiffableTable) -> bool {
    fn same_kind(a: &BlockNode, b: &BlockNode) -> bool {
        match (a, b) {
            (BlockNode::Paragraph(_), BlockNode::Paragraph(_)) => true,
            (BlockNode::Table(_), BlockNode::Table(_)) => true,
            (BlockNode::OpaqueBlock(a), BlockNode::OpaqueBlock(b)) => a.opaque_ref == b.opaque_ref,
            (BlockNode::Paragraph(_), _)
            | (BlockNode::Table(_), _)
            | (BlockNode::OpaqueBlock(_), _) => false,
        }
    }
    // Row/cell counts are guaranteed equal here (same structure, no row changes),
    // so a shorter zip simply means we don't inspect the surplus — which is the
    // very gap we are guarding against; check the lengths explicitly.
    base.table
        .rows
        .iter()
        .zip(target.table.rows.iter())
        .all(|(base_row, target_row)| {
            base_row
                .cells
                .iter()
                .zip(target_row.cells.iter())
                .all(|(base_cell, target_cell)| {
                    base_cell.blocks.len() == target_cell.blocks.len()
                        && base_cell
                            .blocks
                            .iter()
                            .zip(target_cell.blocks.iter())
                            .all(|(b, t)| same_kind(b, t))
                })
        })
}

enum MatchedTableChange {
    StructureChanged,
    CellsModified(Vec<TableCellChange>),
}

fn diff_matched_tables(
    base: &DiffableTable,
    target: &DiffableTable,
) -> Result<MatchedTableChange, String> {
    // Walk base and target TableNode rows/cells in parallel.
    // Same structure_hash guarantees identical row count, cell count per row,
    // grid spans, and vertical merge patterns — but NOT identical block counts
    // per cell: `compute_table_structure_hash` does not hash blocks-per-cell. A
    // cell that gained or lost a paragraph (or whose blocks no longer line up by
    // kind) therefore reaches here, where zipping the cell block lists would
    // truncate the surplus and misalign the rest — silently dropping the change
    // (P0 #4). When that happens we cannot produce a faithful per-cell inline
    // diff, so escalate the whole table to a structural replace: the merge
    // deletes the base table and inserts the target, reproducing the target
    // exactly on accept (coarser than inline, but correct rather than lossy).
    if !cell_block_lists_align(base, target) {
        return Ok(MatchedTableChange::StructureChanged);
    }

    let mut cell_changes: Vec<TableCellChange> = Vec::new();

    for (row_idx, (base_row, target_row)) in base
        .table
        .rows
        .iter()
        .zip(target.table.rows.iter())
        .enumerate()
    {
        for (cell_idx, (base_cell, target_cell)) in base_row
            .cells
            .iter()
            .zip(target_row.cells.iter())
            .enumerate()
        {
            let mut paragraph_changes: Vec<CellParagraphChange> = Vec::new();
            let mut nested_table_diffs: Vec<NestedTableDiff> = Vec::new();

            for (block_idx, (base_block, target_block)) in base_cell
                .blocks
                .iter()
                .zip(target_cell.blocks.iter())
                .enumerate()
            {
                match (base_block, target_block) {
                    (BlockNode::Paragraph(base_para), BlockNode::Paragraph(target_para)) => {
                        let base_inlines = base_para.all_inlines_owned();
                        let target_inlines = target_para.all_inlines_owned();
                        let base_text = extract_inline_text(&base_inlines);
                        let target_text = extract_inline_text(&target_inlines);

                        if base_text != target_text {
                            let inline_changes = diff_block_content_resolving_opaques(
                                &base_inlines,
                                &target_inlines,
                                &HashMap::new(),
                            );
                            paragraph_changes.push(CellParagraphChange {
                                block_index: block_idx,
                                inline_changes,
                                new_block: target_block.clone(),
                            });
                        }
                    }
                    (BlockNode::Table(base_inner), BlockNode::Table(target_inner)) => {
                        if let Some(nested_diff) =
                            diff_nested_tables(base_inner, target_inner, block_idx)?
                        {
                            nested_table_diffs.push(nested_diff);
                        }
                    }
                    (
                        BlockNode::OpaqueBlock(base_opaque),
                        BlockNode::OpaqueBlock(target_opaque),
                    ) => {
                        // cell_block_lists_align's same_kind only lets an
                        // (OpaqueBlock, OpaqueBlock) pair reach this loop when
                        // their opaque_ref already matches (same opaque
                        // identity) — a differing opaque_ref is caught there
                        // and escalates the whole table to
                        // TableStructureChanged before we get here. So a match
                        // here is always identity-equal: no cell change.
                        debug_assert_eq!(
                            base_opaque.opaque_ref, target_opaque.opaque_ref,
                            "cell_block_lists_align must reject differing opaque_ref pairs \
                             before diff_matched_tables zips cell block lists"
                        );
                    }
                    // Mismatched block kinds (paragraph/table/opaque combinations
                    // other than the three above): cell_block_lists_align's
                    // same_kind requires an exact kind match (plus opaque_ref
                    // equality for opaques), so no such pair can reach this loop
                    // — a mismatch there escalates to TableStructureChanged
                    // before diff_matched_tables starts zipping. Enumerated
                    // explicitly (rather than a bare `_`) so a future BlockNode
                    // variant added to same_kind without a matching arm here
                    // panics loudly instead of silently vanishing from the diff.
                    (BlockNode::Paragraph(_), _)
                    | (BlockNode::Table(_), _)
                    | (BlockNode::OpaqueBlock(_), _) => {
                        unreachable!(
                            "cell_block_lists_align guarantees only identical-kind \
                             (and, for opaques, identical opaque_ref) pairs reach \
                             diff_matched_tables' per-cell loop"
                        )
                    }
                }
            }

            let formatting_changed = base_cell.formatting != target_cell.formatting;
            let new_cell_formatting = if formatting_changed {
                Some(target_cell.formatting.clone())
            } else {
                None
            };

            if !paragraph_changes.is_empty() || !nested_table_diffs.is_empty() || formatting_changed
            {
                cell_changes.push(TableCellChange {
                    row_index: row_idx,
                    cell_index: cell_idx,
                    paragraph_changes,
                    nested_table_diffs,
                    new_cell_formatting,
                });
            }
        }
    }

    if cell_changes.is_empty() {
        return Ok(MatchedTableChange::CellsModified(Vec::new()));
    }

    Ok(MatchedTableChange::CellsModified(cell_changes))
}

/// Diff a pair of nested tables within a cell.
///
/// Runs row-level alignment to detect structural changes. If rows were
/// inserted/deleted, returns `NestedTableDiffKind::StructureChanged`. Otherwise,
/// recursively diffs cell content (including deeper nested tables) and returns
/// `NestedTableDiffKind::CellsModified`.
pub fn diff_nested_tables(
    base: &TableNode,
    target: &TableNode,
    block_index: usize,
) -> Result<Option<NestedTableDiff>, String> {
    // Fast path: skip if text is identical.
    let base_text = extract_table_text(base);
    let target_text = extract_table_text(target);
    if base_text == target_text {
        return Ok(None);
    }

    let table_diff = compute_table_diff_result(base, target)?;
    let has_row_changes = table_diff.row_alignment.iter().any(|a| {
        matches!(
            a,
            TableRowAlignment::Inserted { .. } | TableRowAlignment::Deleted { .. }
        )
    });

    if has_row_changes {
        Ok(Some(NestedTableDiff {
            block_index,
            diff: NestedTableDiffKind::StructureChanged {
                table_diff: Box::new(table_diff),
                new_table: Box::new(target.clone()),
            },
        }))
    } else {
        // Same structure — diff cells recursively using DiffableTable wrapper.
        let base_dt = DiffableTable {
            table: base.clone(),
        };
        let target_dt = DiffableTable {
            table: target.clone(),
        };
        match diff_matched_tables(&base_dt, &target_dt)? {
            MatchedTableChange::StructureChanged => Ok(Some(NestedTableDiff {
                block_index,
                diff: NestedTableDiffKind::StructureChanged {
                    table_diff: Box::new(table_diff),
                    new_table: Box::new(target.clone()),
                },
            })),
            MatchedTableChange::CellsModified(cell_changes) if cell_changes.is_empty() => Ok(None),
            MatchedTableChange::CellsModified(cell_changes) => Ok(Some(NestedTableDiff {
                block_index,
                diff: NestedTableDiffKind::CellsModified { cell_changes },
            })),
        }
    }
}

/// Diff inline content within a single block using token-level diffing.
/// This version does not preserve marks (legacy, used for plain text).
pub fn diff_block_content(old_text: &str, new_text: &str) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    let old_tokens = tokenize(old_text);
    let new_tokens = tokenize(new_text);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens);
    let result: Vec<InlineChange> = diff
        .iter_all_changes()
        .map(|change| match change.tag() {
            ChangeTag::Equal => InlineChange::Unchanged {
                text: change.to_string_lossy().into_owned(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            ChangeTag::Insert => InlineChange::Inserted {
                text: change.to_string_lossy().into_owned(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            ChangeTag::Delete => InlineChange::Deleted {
                text: change.to_string_lossy().into_owned(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        })
        .collect();
    cleanup_inline_changes_with_config(result, &heuristics)
}

/// Per-character formatting info: boolean marks + value-carrying style props.
#[derive(Clone, Default)]
struct CharFormatting {
    marks: Vec<Mark>,
    style_props: StyleProps,
}

/// Extract text from inlines along with a per-character formatting mapping.
/// Returns (text, char_fmt) where char_fmt[i] contains the formatting for character i.
fn extract_text_with_marks_and_opaques<'a>(
    inlines: &'a [InlineNode],
) -> (String, Vec<CharFormatting>, Vec<OpaqueTracker<'a>>) {
    let mut text = String::new();
    let mut char_fmt: Vec<CharFormatting> = Vec::new();
    let mut opaques: Vec<OpaqueTracker<'a>> = Vec::new();

    for (inline_index, inline) in inlines.iter().enumerate() {
        match inline {
            InlineNode::Text(t) => {
                let node_text = if t.style_props.caps == MarkValue::On {
                    t.text.to_uppercase()
                } else {
                    t.text.clone()
                };
                let fmt = CharFormatting {
                    marks: t.marks.clone(),
                    style_props: t.style_props.clone(),
                };
                for _ in node_text.chars() {
                    char_fmt.push(fmt.clone());
                }
                text.push_str(&node_text);
            }
            InlineNode::HardBreak(hard_break) => {
                text.push('\n');
                char_fmt.push(CharFormatting {
                    marks: hard_break.wrapper_marks.clone(),
                    style_props: hard_break.wrapper_style_props.clone(),
                });
            }
            InlineNode::OpaqueInline(o) => {
                opaques.push(OpaqueTracker {
                    inline_index,
                    node: o,
                });
                let tag = opaque_diff_tag(o);
                for _ in tag.chars() {
                    char_fmt.push(CharFormatting::default());
                }
                text.push_str(&tag);
            }
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
        }
    }

    (text, char_fmt, opaques)
}

/// Get formatting at a specific character offset, or default if out of bounds.
fn formatting_at_offset(char_fmt: &[CharFormatting], offset: usize) -> CharFormatting {
    char_fmt.get(offset).cloned().unwrap_or_default()
}

/// Get formatting for a token spanning `offset..offset+len`.
///
/// Uses the first character's formatting as the base, but drops caps
/// unless **every** character in the range carries it. Caps is the only mark
/// with a rendering side-effect (uppercase transform), so applying it to a
/// whole token based on a single character produces incorrect text.
fn formatting_for_token_range(
    char_fmt: &[CharFormatting],
    offset: usize,
    len: usize,
) -> CharFormatting {
    let mut fmt = formatting_at_offset(char_fmt, offset);

    // Fast path: single-char token or no caps mark — nothing to reconcile.
    if len <= 1 || fmt.style_props.caps != MarkValue::On {
        return fmt;
    }

    // Check whether ALL characters in the token carry Caps.
    let all_caps = (offset..offset + len).all(|i| {
        char_fmt
            .get(i)
            .is_some_and(|cf| cf.style_props.caps == MarkValue::On)
    });

    if !all_caps {
        fmt.style_props.caps = MarkValue::Inherit;
    }

    fmt
}

/// Detect formatting-only changes between old and new character positions.
///
/// When text is unchanged (ChangeTag::Equal) but marks or style_props differ,
/// returns the current (new) formatting and a FormattingChange capturing the
/// previous (old) formatting. Author/date are left empty; the merge step fills
/// them from RevisionInfo.
fn detect_formatting_change(
    old_fmt: &CharFormatting,
    new_fmt: &CharFormatting,
) -> (Vec<Mark>, StyleProps, Option<FormattingChange>) {
    let changed = old_fmt.marks != new_fmt.marks || old_fmt.style_props != new_fmt.style_props;
    if changed {
        (
            new_fmt.marks.clone(),
            new_fmt.style_props.clone(),
            Some(FormattingChange {
                carrier: crate::domain::RunFormattingChangeCarrier::RunProperties,
                previous_marks: old_fmt.marks.clone(),
                previous_style_props: old_fmt.style_props.clone(),
                // CharFormatting (this whole character-diff pipeline) never
                // tracked per-property rPr authoring provenance — it only
                // ever carried marks/style_props, so there is no "previous"
                // authored-bitset to recover here. Defaulting to
                // "nothing authored" is neutral, not a regression: this
                // path had no such concept before `previous_rpr_authored`
                // existed either. A reject of a formatting change SYNTHESIZED
                // by document comparison (as opposed to authored through
                // SetRunFormatting, which now captures this correctly) may
                // therefore under-restore authored-vs-inherited state — a
                // known, pre-existing gap, not something this fix widens.
                previous_rpr_authored: RunRprAuthored::default(),
                // Placeholder: merge_diff fills from RevisionInfo.
                revision_id: 0,
                identity: 0,
                author: String::new(),
                date: None,
            }),
        )
    } else {
        (old_fmt.marks.clone(), old_fmt.style_props.clone(), None)
    }
}

/// Diff inline content with marks preservation.
/// Uses token-level diffing but maps character positions back to source marks.
fn build_opaque_change(
    tracker: &OpaqueTracker,
    segment_type: InlineChangeSegmentType,
    note_markers: &HashMap<String, String>,
) -> InlineChange {
    let (text, reference_id, field_kind, field_instruction) =
        extract_opaque_metadata(&tracker.node.kind, note_markers, Some(tracker.node));
    InlineChange::Opaque {
        segment_type,
        kind: opaque_kind_to_segment_kind(&tracker.node.kind),
        opaque_id: tracker.node.id.0.to_string(),
        inline_index: tracker.inline_index,
        text,
        reference_id,
        field_kind,
        field_instruction,
        asset_ref: None,
        asset_width_emu: None,
        asset_height_emu: None,
        alt_text: None,
        url: opaque_url(&tracker.node.kind),
        content_hash: tracker.node.content_hash.clone(),
    }
}

/// Diff inline content resolving opaque placeholders back to `InlineChange::Opaque`.
///
/// This local compiler uses identity-bearing `\u{FFFC}` placeholders so it can
/// distinguish different opaque objects inside a region the caller has already
/// selected, then resolves them back to `InlineChange::Opaque` segments. It
/// does not infer correspondence between documents.
pub(crate) fn diff_block_content_resolving_opaques(
    old_inlines: &[InlineNode],
    new_inlines: &[InlineNode],
    note_markers: &HashMap<String, String>,
) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    let (old_text, old_char_fmt, old_opaques) = extract_text_with_marks_and_opaques(old_inlines);
    let (new_text, new_char_fmt, new_opaques) = extract_text_with_marks_and_opaques(new_inlines);

    if should_bail_out_with_config(&old_text, &new_text, &heuristics) {
        return sort_opaque_runs_by_inline_index(build_full_replace(
            old_inlines,
            new_inlines,
            note_markers,
        ));
    }

    let old_tokens = tokenize(&old_text);
    let new_tokens = tokenize(&new_text);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens);

    let mut old_offset = 0usize;
    let mut new_offset = 0usize;
    let mut old_opaque_idx = 0usize;
    let mut new_opaque_idx = 0usize;
    let mut result = Vec::new();

    for change in diff.iter_all_changes() {
        let text = change.to_string_lossy().into_owned();
        let len = text.chars().count();

        if text.starts_with('\u{FFFC}') {
            // Opaque token — resolve to InlineChange::Opaque
            match change.tag() {
                ChangeTag::Equal => {
                    let tracker = &new_opaques[new_opaque_idx];
                    result.push(build_opaque_change(
                        tracker,
                        InlineChangeSegmentType::Equal,
                        note_markers,
                    ));
                    old_opaque_idx += 1;
                    new_opaque_idx += 1;
                    old_offset += len;
                    new_offset += len;
                }
                ChangeTag::Delete => {
                    let tracker = &old_opaques[old_opaque_idx];
                    result.push(build_opaque_change(
                        tracker,
                        InlineChangeSegmentType::Delete,
                        note_markers,
                    ));
                    old_opaque_idx += 1;
                    old_offset += len;
                }
                ChangeTag::Insert => {
                    let tracker = &new_opaques[new_opaque_idx];
                    result.push(build_opaque_change(
                        tracker,
                        InlineChangeSegmentType::Insert,
                        note_markers,
                    ));
                    new_opaque_idx += 1;
                    new_offset += len;
                }
            }
        } else {
            // Regular text — same logic as diff_block_content_with_marks_and_notes
            match change.tag() {
                ChangeTag::Equal => {
                    let old_fmt = formatting_for_token_range(&old_char_fmt, old_offset, len);
                    let new_fmt = formatting_for_token_range(&new_char_fmt, new_offset, len);
                    let (marks, style_props, formatting_change) =
                        detect_formatting_change(&old_fmt, &new_fmt);
                    result.push(InlineChange::Unchanged {
                        text,
                        marks,
                        style_props,
                        formatting_change,
                    });
                    old_offset += len;
                    new_offset += len;
                }
                ChangeTag::Delete => {
                    let fmt = formatting_for_token_range(&old_char_fmt, old_offset, len);
                    result.push(InlineChange::Deleted {
                        text,
                        marks: fmt.marks,
                        style_props: fmt.style_props,
                        formatting_change: None,
                        rev_id: 0,
                    });
                    old_offset += len;
                }
                ChangeTag::Insert => {
                    let fmt = formatting_for_token_range(&new_char_fmt, new_offset, len);
                    result.push(InlineChange::Inserted {
                        text,
                        marks: fmt.marks,
                        style_props: fmt.style_props,
                        formatting_change: None,
                        rev_id: 0,
                    });
                    new_offset += len;
                }
            }
        }
    }

    let result = cleanup_inline_changes_with_config(result, &heuristics);
    sort_opaque_runs_by_inline_index(result)
}

/// Sort consecutive runs of opaque segments by inline_index so they are
/// monotonically non-decreasing. The diff algorithm groups all deletes before
/// inserts within a hunk, which can produce out-of-order inline indices when
/// opaques at multiple positions are replaced (e.g. indices 2,3,2,3 instead
/// of 2,2,3,3).
fn sort_opaque_runs_by_inline_index(mut changes: Vec<InlineChange>) -> Vec<InlineChange> {
    let len = changes.len();
    let mut i = 0;
    while i < len {
        if matches!(changes[i], InlineChange::Opaque { .. }) {
            let start = i;
            while i < len && matches!(changes[i], InlineChange::Opaque { .. }) {
                i += 1;
            }
            if i - start > 1 {
                changes[start..i].sort_by_key(|c| match c {
                    InlineChange::Opaque {
                        inline_index,
                        segment_type,
                        kind,
                        ..
                    } => {
                        let type_order = match (kind, segment_type) {
                            (_, InlineChangeSegmentType::Equal) => 0,
                            (OpaqueSegmentKind::Drawing, InlineChangeSegmentType::Insert) => 1,
                            (OpaqueSegmentKind::Drawing, InlineChangeSegmentType::Delete) => 2,
                            (_, InlineChangeSegmentType::Delete) => 1,
                            (_, InlineChangeSegmentType::Insert) => 2,
                        };
                        (*inline_index, type_order)
                    }
                    _ => unreachable!(),
                });
            }
        } else {
            i += 1;
        }
    }
    changes
}

// =============================================================================
// Layer 3: Early bail-out
// =============================================================================

/// Check if a token looks like a high-value numeric/financial token.
fn is_high_value_token(s: &str) -> bool {
    // Numbers, percentages, currency, section references
    s.chars().any(|c| c.is_ascii_digit())
        || s == "%"
        || s == "$"
        || s.eq_ignore_ascii_case("USD")
        || s.eq_ignore_ascii_case("EUR")
        || s.eq_ignore_ascii_case("GBP")
}

/// Compute content-only similarity (ignoring whitespace tokens).
/// Whitespace tokens inflate standard text_similarity, making bail-out
/// hard to trigger for truly different texts.
fn content_similarity(old_text: &str, new_text: &str) -> f64 {
    let norm1 = normalize_for_similarity(old_text);
    let norm2 = normalize_for_similarity(new_text);
    let old_tokens: Vec<&str> = tokenize(&norm1)
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .collect();
    let new_tokens: Vec<&str> = tokenize(&norm2)
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .collect();
    if old_tokens.is_empty() && new_tokens.is_empty() {
        return 1.0;
    }
    if old_tokens.is_empty() || new_tokens.is_empty() {
        return 0.0;
    }
    TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens)
        .ratio() as f64
}

/// Check if we should bail out of token-level diffing.
/// Returns true if texts are so dissimilar that a clean delete-all/insert-all is better.
/// Safety: never bails out if high-value tokens (numbers, currency, percentages) differ.
fn should_bail_out_with_config(
    old_text: &str,
    new_text: &str,
    heuristics: &DiffHeuristics,
) -> bool {
    // Only consider bail-out for sufficiently long texts
    if old_text.chars().count() < heuristics.bail_out_min_chars
        && new_text.chars().count() < heuristics.bail_out_min_chars
    {
        return false;
    }

    let sim = content_similarity(old_text, new_text);
    if sim >= heuristics.bail_out_similarity_threshold {
        return false;
    }

    // Safety check: scan for differing high-value tokens
    let old_tokens = tokenize(old_text);
    let new_tokens = tokenize(new_text);

    let old_hv: std::collections::HashSet<&str> = old_tokens
        .iter()
        .copied()
        .filter(|t| is_high_value_token(t))
        .collect();
    let new_hv: std::collections::HashSet<&str> = new_tokens
        .iter()
        .copied()
        .filter(|t| is_high_value_token(t))
        .collect();

    // If any high-value tokens differ, don't bail out — preserve inline visibility
    if old_hv != new_hv {
        return false;
    }

    true
}

/// Build a clean delete-all + insert-all replacement from inlines, preserving marks.
/// Common leading/trailing segments are factored out as Unchanged.
fn build_full_replace(
    old_inlines: &[InlineNode],
    new_inlines: &[InlineNode],
    note_markers: &HashMap<String, String>,
) -> Vec<InlineChange> {
    let mut result = inlines_to_segments(old_inlines, "delete", note_markers);
    result.extend(inlines_to_segments(new_inlines, "insert", note_markers));
    factor_common_affixes(result)
}

// =============================================================================
// Layer 2: Post-processing cleanup (zipper collapse)
// =============================================================================

/// Count non-whitespace characters in a string.
fn non_ws_chars(s: &str) -> usize {
    s.chars().filter(|c| !c.is_whitespace()).count()
}

/// Get the text content of an InlineChange.
fn inline_change_text(change: &InlineChange) -> &str {
    match change {
        InlineChange::Unchanged { text, .. } => text,
        InlineChange::Deleted { text, .. } => text,
        InlineChange::Inserted { text, .. } => text,
        InlineChange::Opaque { text, .. } => text.as_deref().unwrap_or(""),
    }
}

/// Get the marks of an InlineChange.
fn inline_change_marks(change: &InlineChange) -> &[Mark] {
    match change {
        InlineChange::Unchanged { marks, .. } => marks,
        InlineChange::Deleted { marks, .. } => marks,
        InlineChange::Inserted { marks, .. } => marks,
        InlineChange::Opaque { .. } => &[],
    }
}

/// Get the style_props of an InlineChange.
/// Panics on Opaque (which has no style_props); callers must filter those out.
fn inline_change_style_props(change: &InlineChange) -> &StyleProps {
    match change {
        InlineChange::Unchanged { style_props, .. }
        | InlineChange::Deleted { style_props, .. }
        | InlineChange::Inserted { style_props, .. } => style_props,
        InlineChange::Opaque { .. } => {
            panic!("inline_change_style_props called on Opaque segment")
        }
    }
}

/// Check if an InlineChange is an enumerator token like (i), (ii), (a) etc.
fn is_enumerator_anchor(change: &InlineChange) -> bool {
    let text = inline_change_text(change);
    if !text.starts_with('(') || !text.ends_with(')') || text.len() < 3 {
        return false;
    }
    let inner = &text[1..text.len() - 1];
    is_enumerator_content(inner)
}

/// Merge adjacent same-type segments only when their formatting payload matches.
fn merge_adjacent_same_type(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    if changes.is_empty() {
        return changes;
    }

    let mut result: Vec<InlineChange> = Vec::with_capacity(changes.len());

    for change in changes {
        if matches!(change, InlineChange::Opaque { .. }) {
            result.push(change);
            continue;
        }

        let should_merge = if let Some(last) = result.last() {
            !matches!(last, InlineChange::Opaque { .. })
                && inline_change_text(last) != "\n"
                && inline_change_text(&change) != "\n"
                && std::mem::discriminant(last) == std::mem::discriminant(&change)
                && inline_change_marks(last) == inline_change_marks(&change)
                && match (last, &change) {
                    (
                        InlineChange::Unchanged {
                            style_props: last_style,
                            formatting_change: last_fc,
                            ..
                        },
                        InlineChange::Unchanged {
                            style_props: change_style,
                            formatting_change: change_fc,
                            ..
                        },
                    ) => last_style == change_style && last_fc == change_fc,
                    (
                        InlineChange::Deleted {
                            style_props: last_style,
                            ..
                        },
                        InlineChange::Deleted {
                            style_props: change_style,
                            ..
                        },
                    ) => last_style == change_style,
                    (
                        InlineChange::Inserted {
                            style_props: last_style,
                            ..
                        },
                        InlineChange::Inserted {
                            style_props: change_style,
                            ..
                        },
                    ) => last_style == change_style,
                    _ => false,
                }
        } else {
            false
        };

        if should_merge {
            // Merge into the last element — should_merge is only true when result is non-empty
            let Some(last) = result.last_mut() else {
                unreachable!("should_merge requires non-empty result");
            };
            match last {
                InlineChange::Unchanged { text, .. } => {
                    text.push_str(inline_change_text(&change));
                }
                InlineChange::Deleted { text, .. } => {
                    text.push_str(inline_change_text(&change));
                }
                InlineChange::Inserted { text, .. } => {
                    text.push_str(inline_change_text(&change));
                }
                InlineChange::Opaque { .. } => {}
            }
        } else {
            result.push(change);
        }
    }

    result
}

/// Identify indices of "strong anchors" — Unchanged spans with >= ANCHOR_MIN_CHARS
/// non-whitespace characters, OR enumerator tokens like (i), (ii).
fn find_strong_anchors_with_config(
    changes: &[InlineChange],
    heuristics: &DiffHeuristics,
) -> Vec<usize> {
    changes
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if (matches!(c, InlineChange::Unchanged { .. })
                && (non_ws_chars(inline_change_text(c)) >= heuristics.anchor_min_chars
                    || is_enumerator_anchor(c)
                    // Unchanged text containing U+FFFC opaque placeholders must also be
                    // treated as strong anchors. When diff_block_content_with_marks is
                    // used (instead of diff_block_content_resolving_opaques), opaques
                    // appear as bare U+FFFC inside Unchanged text rather than as
                    // InlineChange::Opaque. Collapsing such a region duplicates the
                    // placeholder into both del and ins sides, causing the opaque element
                    // to appear twice (once deleted, once inserted) instead of once as
                    // Normal — which breaks accept/reject text parity.
                    || inline_change_text(c).contains('\u{FFFC}')))
                // Opaques are structural barriers regardless of change type.
                // collapse_region converts them to fallback display text, which loses
                // the identity and position required to merge the actual package node.
                || matches!(c, InlineChange::Opaque { .. })
            {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

/// Count alternating del/ins groups in a region.
/// Each contiguous block of Deleted tokens is one group; each contiguous block
/// of Inserted tokens is one group. Tiny unchanged spans (< 3 non-ws chars)
/// are ignored (treated as transparent). This counts the number of such groups.
fn count_change_runs(region: &[InlineChange]) -> usize {
    let mut runs = 0;
    // Track whether last significant segment was Del, Ins, or Equal
    // 0 = none, 1 = deleted, 2 = inserted
    let mut last_type: u8 = 0;

    for change in region {
        match change {
            InlineChange::Deleted { .. } => {
                if last_type != 1 {
                    runs += 1;
                }
                last_type = 1;
            }
            InlineChange::Inserted { .. } => {
                if last_type != 2 {
                    runs += 1;
                }
                last_type = 2;
            }
            InlineChange::Unchanged { text, .. } => {
                // Only reset if this is a substantial unchanged span
                if non_ws_chars(text) >= 3 {
                    last_type = 0;
                }
                // Otherwise ignore (transparent)
            }
            InlineChange::Opaque {
                segment_type, text, ..
            } => {
                let mapped = match segment_type {
                    InlineChangeSegmentType::Equal => 0,
                    InlineChangeSegmentType::Delete => 1,
                    InlineChangeSegmentType::Insert => 2,
                };
                if mapped == 0 {
                    if non_ws_chars(text.as_deref().unwrap_or("")) >= 3 {
                        last_type = 0;
                    }
                } else if last_type != mapped {
                    runs += 1;
                    last_type = mapped;
                }
            }
        }
    }

    runs
}

/// Count how many tokens in a region are changed (Deleted or Inserted).
fn count_changed_tokens(region: &[InlineChange]) -> usize {
    region
        .iter()
        .filter(|c| {
            matches!(
                c,
                InlineChange::Deleted { .. }
                    | InlineChange::Inserted { .. }
                    | InlineChange::Opaque {
                        segment_type: InlineChangeSegmentType::Delete
                            | InlineChangeSegmentType::Insert,
                        ..
                    }
            )
        })
        .count()
}

/// Compute the text similarity of just the old-side and new-side of a region.
fn region_similarity(region: &[InlineChange]) -> f64 {
    let old_text: String = region
        .iter()
        .filter_map(|c| match c {
            InlineChange::Unchanged { text, .. } | InlineChange::Deleted { text, .. } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    let new_text: String = region
        .iter()
        .filter_map(|c| match c {
            InlineChange::Unchanged { text, .. } | InlineChange::Inserted { text, .. } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    text_similarity(&old_text, &new_text)
}

/// Decide whether a region between strong anchors should be collapsed.
fn should_collapse_region_with_config(
    region: &[InlineChange],
    heuristics: &DiffHeuristics,
) -> bool {
    // Check if region has any changes at all
    let changed = count_changed_tokens(region);
    if changed == 0 {
        return false;
    }

    let runs = count_change_runs(region);
    let total = region.len();

    // High alternation: many runs relative to region size
    if runs >= heuristics.zipper_min_change_runs
        && total > 0
        && (runs as f64 / total as f64) >= 0.20
    {
        return true;
    }

    // Low similarity: force collapse even without high alternation
    if region_similarity(region) < heuristics.low_similarity_threshold {
        return true;
    }

    false
}

/// Factor out matching leading/trailing segments from a DEL+INS sequence as Unchanged.
///
/// Given a `Vec<InlineChange>` containing Deleted segments followed by Inserted segments
/// (as produced by `collapse_region` or `build_full_replace`), compare the del and ins
/// segment lists. Leading segments that match (same text and marks) become Unchanged
/// (prefix), trailing matching segments become Unchanged (suffix), and the differing
/// middle remains Deleted/Inserted.
fn factor_common_affixes(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    // Split into del and ins segments.
    let mut del: Vec<&InlineChange> = Vec::new();
    let mut ins: Vec<&InlineChange> = Vec::new();
    for c in &changes {
        match c {
            InlineChange::Deleted { .. } => del.push(c),
            InlineChange::Inserted { .. } => ins.push(c),
            // Pass through unchanged/opaque unmodified.
            _ => {}
        }
    }

    if del.is_empty() || ins.is_empty() {
        return changes;
    }

    let segs_match = |d: &&InlineChange, i: &&InlineChange| {
        inline_change_text(d) == inline_change_text(i)
            && inline_change_marks(d) == inline_change_marks(i)
            && inline_change_style_props(d) == inline_change_style_props(i)
    };

    let prefix_len = del
        .iter()
        .zip(ins.iter())
        .take_while(|(d, i)| segs_match(d, i))
        .count();

    let suffix_len = del[prefix_len..]
        .iter()
        .rev()
        .zip(ins[prefix_len..].iter().rev())
        .take_while(|(d, i)| segs_match(d, i))
        .count();

    if prefix_len == 0 && suffix_len == 0 {
        return changes;
    }

    let del_mid_end = del.len() - suffix_len;
    let ins_mid_end = ins.len() - suffix_len;

    let mut result = Vec::new();

    // Prefix → Unchanged
    for d in &del[..prefix_len] {
        result.push(InlineChange::Unchanged {
            text: inline_change_text(d).to_string(),
            marks: inline_change_marks(d).to_vec(),
            style_props: inline_change_style_props(d).clone(),
            formatting_change: None,
        });
    }

    // Middle del
    for d in &del[prefix_len..del_mid_end] {
        result.push((*d).clone());
    }

    // Middle ins
    for i in &ins[prefix_len..ins_mid_end] {
        result.push((*i).clone());
    }

    // Suffix → Unchanged
    for d in &del[del_mid_end..] {
        result.push(InlineChange::Unchanged {
            text: inline_change_text(d).to_string(),
            marks: inline_change_marks(d).to_vec(),
            style_props: inline_change_style_props(d).clone(),
            formatting_change: None,
        });
    }

    factor_char_level_affixes(result)
}

/// True if byte position `pos` in `text` falls on a token boundary
/// (the tokenizer would start a new token here).
fn is_token_boundary(text: &str, pos: usize) -> bool {
    if pos == 0 || pos >= text.len() {
        return true;
    }
    if !text.is_char_boundary(pos) {
        return false;
    }
    let prev = char_class(text[..pos].chars().next_back().unwrap());
    let curr = char_class(text[pos..].chars().next().unwrap());
    prev != curr || matches!(prev, CharClass::Punctuation) || matches!(curr, CharClass::Punctuation)
}

/// Snap a byte-level common prefix length down to a token boundary in both texts.
fn snap_prefix_to_token_boundary(old_text: &str, new_text: &str, raw: usize) -> usize {
    if raw == 0 {
        return 0;
    }
    let min_len = old_text.len().min(new_text.len());
    if raw >= min_len {
        return raw;
    }
    // Within 0..raw, bytes are identical so char boundaries match in both.
    let mut pos = raw;
    while pos > 0 && !old_text.is_char_boundary(pos) {
        pos -= 1;
    }
    while pos > 0 {
        if is_token_boundary(old_text, pos) && is_token_boundary(new_text, pos) {
            return pos;
        }
        pos -= 1;
        while pos > 0 && !old_text.is_char_boundary(pos) {
            pos -= 1;
        }
    }
    0
}

/// Snap a byte-level common suffix length down to a token boundary in both texts.
fn snap_suffix_to_token_boundary(old_text: &str, new_text: &str, raw_suffix: usize) -> usize {
    if raw_suffix == 0 {
        return 0;
    }
    let min_len = old_text.len().min(new_text.len());
    if raw_suffix >= min_len {
        return raw_suffix;
    }
    let old_start = old_text.len() - raw_suffix;
    let new_start = new_text.len() - raw_suffix;
    let mut offset = 0;
    while offset < raw_suffix {
        if old_text.is_char_boundary(old_start + offset)
            && new_text.is_char_boundary(new_start + offset)
        {
            break;
        }
        offset += 1;
    }
    while offset < raw_suffix {
        let old_pos = old_start + offset;
        let new_pos = new_start + offset;
        if is_token_boundary(old_text, old_pos) && is_token_boundary(new_text, new_pos) {
            return raw_suffix - offset;
        }
        offset += 1;
        while offset < raw_suffix && !old_text.is_char_boundary(old_start + offset) {
            offset += 1;
        }
    }
    0
}

/// Post-processing step: factor character-level common prefix/suffix from adjacent
/// Del/Ins pairs that have matching marks but different text.
///
/// After segment-level `factor_common_affixes`, collapsed regions may contain a single
/// Del + single Ins whose texts share significant prefix/suffix text (e.g.,
/// `Del("five (5) years.")` + `Ins("two (2) years.")`). Segment-level factoring can't
/// help because the entire text differs as a segment. This function recovers the shared
/// text at character level, snapped to token boundaries, producing finer-grained output:
/// `Unchanged(") years.") + Del("five (5") + Ins("two (2") + Unchanged(") years.")`.
fn factor_char_level_affixes(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < changes.len() {
        // Look for adjacent Del/Ins pair.
        if i + 1 < changes.len()
            && let (
                InlineChange::Deleted {
                    text: del_text,
                    marks: del_marks,
                    style_props: del_sp,
                    ..
                },
                InlineChange::Inserted {
                    text: ins_text,
                    marks: ins_marks,
                    style_props: ins_sp,
                    ..
                },
            ) = (&changes[i], &changes[i + 1])
        {
            // Only factor when marks and style_props match (content substitution,
            // not a formatting change).
            if del_marks == ins_marks && del_sp == ins_sp && del_text != ins_text {
                let raw_prefix = del_text
                    .bytes()
                    .zip(ins_text.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                let raw_suffix = del_text.as_bytes()[raw_prefix..]
                    .iter()
                    .rev()
                    .zip(ins_text.as_bytes()[raw_prefix..].iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();

                let prefix = snap_prefix_to_token_boundary(del_text, ins_text, raw_prefix);
                let suffix = snap_suffix_to_token_boundary(del_text, ins_text, raw_suffix);

                let shared = prefix + suffix;
                let del_unique = del_text.len().saturating_sub(shared);
                let ins_unique = ins_text.len().saturating_sub(shared);
                let unique = del_unique.min(ins_unique);

                // Only split when shared text exceeds unique text (the same threshold
                // the granularity invariant test uses) and there's actually unique
                // content left on both sides.
                if shared > unique && del_unique > 0 && ins_unique > 0 && shared >= 4 {
                    let del_mid = &del_text[prefix..del_text.len() - suffix];
                    let ins_mid = &ins_text[prefix..ins_text.len() - suffix];

                    if prefix > 0 {
                        result.push(InlineChange::Unchanged {
                            text: del_text[..prefix].to_string(),
                            marks: del_marks.clone(),
                            style_props: del_sp.clone(),
                            formatting_change: None,
                        });
                    }
                    result.push(InlineChange::Deleted {
                        text: del_mid.to_string(),
                        marks: del_marks.clone(),
                        style_props: del_sp.clone(),
                        formatting_change: None,
                        rev_id: 0,
                    });
                    result.push(InlineChange::Inserted {
                        text: ins_mid.to_string(),
                        marks: ins_marks.clone(),
                        style_props: ins_sp.clone(),
                        formatting_change: None,
                        rev_id: 0,
                    });
                    if suffix > 0 {
                        result.push(InlineChange::Unchanged {
                            text: del_text[del_text.len() - suffix..].to_string(),
                            marks: del_marks.clone(),
                            style_props: del_sp.clone(),
                            formatting_change: None,
                        });
                    }

                    i += 2;
                    continue;
                }
            }
        }

        result.push(changes[i].clone());
        i += 1;
    }

    result
}

/// Collapse a region: gather all old-side text as Deleted, all new-side text as Inserted.
/// Unchanged text within the region is duplicated to both sides.
/// Preserves mark boundaries: emits one segment per contiguous run of same marks,
/// so per-word formatting (e.g. bold defined terms) is not lost.
fn collapse_region(region: &[InlineChange]) -> Vec<InlineChange> {
    // Intermediate segment: text + marks + style_props for one contiguous run.
    struct Seg {
        text: String,
        marks: Vec<Mark>,
        style_props: StyleProps,
    }

    let mut del_segs: Vec<Seg> = Vec::new();
    let mut ins_segs: Vec<Seg> = Vec::new();

    // Push text into a segment list, merging with the last segment only when
    // both toggle marks and value-carrying style props match. Otherwise we can
    // smear formatting across adjacent text/opaque boundaries (e.g. field
    // result text next to a field structural placeholder).
    fn push_seg(segs: &mut Vec<Seg>, text: &str, marks: &[Mark], style_props: &StyleProps) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = segs.last_mut()
            && last.marks == marks
            && last.style_props == *style_props
        {
            last.text.push_str(text);
            return;
        }
        segs.push(Seg {
            text: text.to_string(),
            marks: marks.to_vec(),
            style_props: style_props.clone(),
        });
    }

    for change in region {
        match change {
            InlineChange::Unchanged {
                text,
                marks,
                style_props,
                ..
            } => {
                push_seg(&mut del_segs, text, marks, style_props);
                push_seg(&mut ins_segs, text, marks, style_props);
            }
            InlineChange::Deleted {
                text,
                marks,
                style_props,
                ..
            } => {
                push_seg(&mut del_segs, text, marks, style_props);
            }
            InlineChange::Inserted {
                text,
                marks,
                style_props,
                ..
            } => {
                push_seg(&mut ins_segs, text, marks, style_props);
            }
            InlineChange::Opaque {
                segment_type, text, ..
            } => {
                // Opaque segments have no marks; use empty marks/default style.
                let empty_marks: &[Mark] = &[];
                let default_sp = StyleProps::default();
                match segment_type {
                    InlineChangeSegmentType::Equal => {
                        if let Some(t) = text {
                            push_seg(&mut del_segs, t, empty_marks, &default_sp);
                            push_seg(&mut ins_segs, t, empty_marks, &default_sp);
                        }
                    }
                    InlineChangeSegmentType::Delete => {
                        if let Some(t) = text {
                            push_seg(&mut del_segs, t, empty_marks, &default_sp);
                        }
                    }
                    InlineChangeSegmentType::Insert => {
                        if let Some(t) = text {
                            push_seg(&mut ins_segs, t, empty_marks, &default_sp);
                        }
                    }
                }
            }
        }
    }

    let mut result = Vec::new();
    for seg in del_segs {
        result.push(InlineChange::Deleted {
            text: seg.text,
            marks: seg.marks,
            style_props: seg.style_props,
            formatting_change: None,
            rev_id: 0,
        });
    }
    for seg in ins_segs {
        result.push(InlineChange::Inserted {
            text: seg.text,
            marks: seg.marks,
            style_props: seg.style_props,
            formatting_change: None,
            rev_id: 0,
        });
    }
    factor_common_affixes(result)
}

/// Split changes by strong anchors, collapse qualifying regions.
#[cfg(test)]
#[allow(dead_code)]
fn collapse_zipper_regions(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    collapse_zipper_regions_with_config(changes, &heuristics)
}

fn collapse_zipper_regions_with_config(
    changes: Vec<InlineChange>,
    heuristics: &DiffHeuristics,
) -> Vec<InlineChange> {
    let anchors = find_strong_anchors_with_config(&changes, heuristics);

    if anchors.is_empty() {
        // No strong anchors: treat entire sequence as one region
        if should_collapse_region_with_config(&changes, heuristics) {
            return collapse_region(&changes);
        }
        return changes;
    }

    let mut result = Vec::new();
    let mut prev_end = 0;

    for &anchor_idx in &anchors {
        // Process region before this anchor
        if prev_end < anchor_idx {
            let region = &changes[prev_end..anchor_idx];
            if should_collapse_region_with_config(region, heuristics) {
                result.extend(collapse_region(region));
            } else {
                result.extend_from_slice(region);
            }
        }
        // Emit the anchor itself
        result.push(changes[anchor_idx].clone());
        prev_end = anchor_idx + 1;
    }

    // Process region after last anchor
    if prev_end < changes.len() {
        let region = &changes[prev_end..];
        if should_collapse_region_with_config(region, heuristics) {
            result.extend(collapse_region(region));
        } else {
            result.extend_from_slice(region);
        }
    }

    result
}

/// Main cleanup pipeline for inline changes.
/// 1. Merge adjacent same-type segments
/// 2. Collapse zipper regions between strong anchors
/// 3. Merge again after collapse
pub fn cleanup_inline_changes(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    cleanup_inline_changes_with_config(changes, &heuristics)
}

fn cleanup_inline_changes_with_config(
    changes: Vec<InlineChange>,
    heuristics: &DiffHeuristics,
) -> Vec<InlineChange> {
    let merged = merge_adjacent_same_type(changes);
    let collapsed = collapse_zipper_regions_with_config(merged, heuristics);
    let merged = merge_adjacent_same_type(collapsed);
    factor_char_level_affixes(merged)
}

// =============================================================================
// Story diffing functions
// =============================================================================
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
/// Convert a block's inline nodes to InlineChange segments of a given type.
///
/// Walks InlineNode list, extracting TextNode.text + TextNode.marks,
/// and mapping HardBreak to "\n".
///
/// `note_markers` maps (kind_prefix, reference_id) → marker_text for footnotes/endnotes/comments.
/// Keys use prefixes: "fn:{id}", "en:{id}", "cm:{id}".
pub fn inlines_to_segments(
    inlines: &[InlineNode],
    change_type: &str,
    note_markers: &HashMap<String, String>,
) -> Vec<InlineChange> {
    let mut segments = Vec::new();
    for (inline_index, inline) in inlines.iter().enumerate() {
        match inline {
            InlineNode::Text(t) => {
                let seg = match change_type {
                    "insert" => InlineChange::Inserted {
                        text: t.text.clone(),
                        marks: t.marks.clone(),
                        style_props: t.style_props.clone(),
                        formatting_change: t.formatting_change.clone(),
                        rev_id: 0,
                    },
                    "delete" => InlineChange::Deleted {
                        text: t.text.clone(),
                        marks: t.marks.clone(),
                        style_props: t.style_props.clone(),
                        formatting_change: t.formatting_change.clone(),
                        rev_id: 0,
                    },
                    _ => InlineChange::Unchanged {
                        text: t.text.clone(),
                        marks: t.marks.clone(),
                        style_props: t.style_props.clone(),
                        formatting_change: t.formatting_change.clone(),
                    },
                };
                segments.push(seg);
            }
            InlineNode::HardBreak(_) => {
                let seg = match change_type {
                    "insert" => InlineChange::Inserted {
                        text: "\n".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    "delete" => InlineChange::Deleted {
                        text: "\n".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    _ => InlineChange::Unchanged {
                        text: "\n".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                };
                segments.push(seg);
            }
            InlineNode::OpaqueInline(o) => {
                let (text, reference_id, field_kind, field_instruction) =
                    extract_opaque_metadata(&o.kind, note_markers, Some(o));
                segments.push(InlineChange::Opaque {
                    segment_type: change_type_to_segment_type(change_type),
                    kind: opaque_kind_to_segment_kind(&o.kind),
                    opaque_id: o.id.0.to_string(),
                    inline_index,
                    text,
                    reference_id,
                    field_kind,
                    field_instruction,
                    asset_ref: None, // Populated later by enrich_segments_with_assets.
                    asset_width_emu: None,
                    asset_height_emu: None,
                    alt_text: None,
                    url: opaque_url(&o.kind),
                    content_hash: o.content_hash.clone(),
                });
            }
            // Comment anchor markers (§17.13.4). These are zero-width — they
            // carry NO text and don't advance the offset — but a redline-review
            // frontend needs them to LOCATE the commented span: it pairs the
            // start marker with the end-reference (matched by `reference_id`,
            // which is the comment's `w:id` = the `CommentPayload.id`) and
            // highlights the text between. We surface them as opaque segments of
            // kind `CommentReference` carrying that `reference_id`, mirroring how
            // footnote/endnote references are projected. `CommentRangeStart`
            // marks the span open; `CommentReference` (emitted at the span's end
            // by the engine) marks the close. `CommentRangeEnd` is the redundant
            // structural twin of `CommentReference` and is dropped to avoid a
            // double close marker.
            InlineNode::CommentRangeStart { id } | InlineNode::CommentReference { id } => {
                segments.push(InlineChange::Opaque {
                    segment_type: change_type_to_segment_type(change_type),
                    kind: OpaqueSegmentKind::CommentReference,
                    opaque_id: id.clone(),
                    inline_index,
                    text: None,
                    reference_id: Some(id.clone()),
                    field_kind: None,
                    field_instruction: None,
                    asset_ref: None,
                    asset_width_emu: None,
                    asset_height_emu: None,
                    alt_text: None,
                    url: None,
                    content_hash: None,
                });
            }
            // Skip decorations and the redundant comment range-end marker
            // (the range-start + comment-reference pair already bracket the span).
            InlineNode::Decoration(_) | InlineNode::CommentRangeEnd { .. } => {}
        }
    }
    // Word often splits text across multiple XML runs arbitrarily (editing
    // history, spell-check, etc.). Merge adjacent same-type segments so
    // inserted/deleted whole-blocks don't produce spurious span splits.
    merge_adjacent_same_type(segments)
}

fn extract_opaque_metadata(
    kind: &OpaqueKind,
    note_markers: &HashMap<String, String>,
    opaque_node: Option<&OpaqueInlineNode>,
) -> (
    Option<String>,
    Option<String>,
    Option<crate::domain::FieldKind>,
    Option<String>,
) {
    match kind {
        OpaqueKind::Hyperlink(data) if !data.text.is_empty() => {
            (Some(data.text.clone()), None, None, None)
        }
        OpaqueKind::FootnoteReference(ref_data) => {
            let marker = note_markers
                .get(&format!("fn:{}", ref_data.reference_id))
                .cloned();
            (marker, Some(ref_data.reference_id.clone()), None, None)
        }
        OpaqueKind::EndnoteReference(ref_data) => {
            let marker = note_markers
                .get(&format!("en:{}", ref_data.reference_id))
                .cloned();
            (marker, Some(ref_data.reference_id.clone()), None, None)
        }
        OpaqueKind::CommentReference(ref_data) => {
            let marker = note_markers
                .get(&format!("cm:{}", ref_data.reference_id))
                .cloned();
            (marker, Some(ref_data.reference_id.clone()), None, None)
        }
        OpaqueKind::Field(field_data) => {
            let text = field_data.result_text.clone();
            // Prefer the canonical instruction text reconstructed from the
            // typed semantic — this is whitespace-invariant, so MERGEFIELD
            // reformatting (\* MERGEFORMAT spacing, etc.) no longer
            // produces a phantom diff. Fragments without a parsed semantic
            // (e.g. mid-run instrText slices) fall back to the raw bytes.
            let field_instruction = field_data
                .semantic
                .as_ref()
                .map(|s| s.to_instruction_text())
                .or_else(|| field_data.instruction_text.clone());
            (
                text,
                None,
                Some(field_data.field_kind.clone()),
                field_instruction,
            )
        }
        OpaqueKind::Drawing => {
            // Try descriptive fallback text from the drawing XML, otherwise
            // use a sentinel matching the atom path (opaque_sentinel in changelet.rs).
            let text = opaque_node
                .and_then(|o| o.raw_xml.as_deref())
                .and_then(extract_drawing_fallback_text)
                .or_else(|| Some("[image]".to_string()));
            (text, None, None, None)
        }
        OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline => {
            // Sentinel matching atom path (opaque_sentinel in changelet.rs).
            (Some("[equation]".to_string()), None, None, None)
        }
        OpaqueKind::Sym(sym_data) => {
            // Display the decoded character from the symbol font
            (Some(sym_data.display_char.to_string()), None, None, None)
        }
        OpaqueKind::Ptab => {
            // Absolute position tab renders as a tab character
            (Some("\t".to_string()), None, None, None)
        }
        _ => (None, None, None, None),
    }
}
