//! Legacy v3 markup parser, kept as a test-construction convenience.
//!
//! Extracted from `edit/mod.rs` so the authoring-grammar core stays focused.
//! Nothing in the engine or HTTP layer depends on this; tests build
//! `ParagraphContent` from a markup string via [`parse_paragraph_markup`].
//! See `edit/AGENTS.md` for the module layout.

use super::{ContentFragment, InlineMarkSet, ParagraphContent};
use crate::domain::NodeId;

// ─── Markup parser (test builder) ────────────────────────────────────────────
//
// `parse_paragraph_markup` is the legacy v3 wire-format parser kept solely as
// a test-construction convenience. The LLM-facing wire format is now the v4
// typed-tree grammar (`edit_v4.rs`); the engine receives `ParagraphContent`
// directly from the v4 adapter. Tests that build `ParagraphContent` from a
// markup string (`"<bold>x</bold>"`) use this parser; nothing in the engine
// or HTTP layer depends on it.

/// Error from parsing the legacy LLM-facing markup text. Used only by tests
/// that build `ParagraphContent` from a markup string convenience.
#[derive(Clone, Debug)]
pub enum MarkupParseError {
    /// An `<opaque .../>` tag is missing the `id` attribute.
    OpaqueMissingId { position: usize },

    /// A `<` was found but doesn't match any known tag pattern.
    UnrecognizedTag { position: usize, snippet: String },

    /// An `<opaque` tag was opened but never closed with `/>`.
    UnterminatedTag { position: usize },

    /// A mark closing tag did not match the currently open mark.
    /// Example: `<bold><italic>foo</bold></italic>`.
    MismatchedCloseTag {
        position: usize,
        expected: &'static str,
        found: &'static str,
    },

    /// An opening mark tag was never closed.
    /// Example: `<bold>foo` with no `</bold>`.
    UnclosedMarkTag { tag: &'static str, position: usize },
}

impl std::fmt::Display for MarkupParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarkupParseError::OpaqueMissingId { position } => {
                write!(f, "position {position}: <opaque> tag missing id attribute")
            }
            MarkupParseError::UnrecognizedTag { position, snippet } => {
                write!(f, "position {position}: unrecognized tag: {snippet}")
            }
            MarkupParseError::UnterminatedTag { position } => {
                write!(f, "position {position}: unterminated tag (missing />)")
            }
            MarkupParseError::MismatchedCloseTag {
                position,
                expected,
                found,
            } => write!(
                f,
                "position {position}: mismatched close tag: expected </{expected}>, found </{found}>"
            ),
            MarkupParseError::UnclosedMarkTag { tag, position } => {
                write!(f, "position {position}: unclosed <{tag}> tag")
            }
        }
    }
}

impl std::error::Error for MarkupParseError {}

const MARK_TAG_NAMES: &[&str] = &[
    "bold",
    "italic",
    "underline",
    "strike",
    "subscript",
    "superscript",
];

/// Return the canonical tag name if `s` matches one of the universal
/// mark tags (case-sensitive).
fn match_mark_tag(s: &str) -> Option<&'static str> {
    MARK_TAG_NAMES.iter().copied().find(|&n| n == s)
}

/// Apply a single mark tag to the in-progress `InlineMarkSet`. `on = true`
/// for opening tags, `on = false` for closing tags.
fn set_mark(set: &mut InlineMarkSet, name: &str, on: bool) {
    match name {
        "bold" => set.bold = on,
        "italic" => set.italic = on,
        "underline" => set.underline = on,
        "strike" => set.strike = on,
        "subscript" => set.subscript = on,
        "superscript" => set.superscript = on,
        _ => {}
    }
}

/// Parse LLM-facing markup text into `ParagraphContent`.
///
/// Supports:
/// - Plain text → `ContentFragment::Text`
/// - `<opaque id="..."/>` / `<anchor id="..."/>` → preserved inline reference
/// - Universal marks (`<bold>`, `<italic>`, `<underline>`, `<strike>`,
///   `<subscript>`, `<superscript>`) → `ContentFragment::StyledText` spans
///   carrying the union of every currently-open mark. Marks nest and
///   close balanced: `<bold><italic>foo</italic> bar</bold>` is valid.
///
/// Hyperlink creation supported via `<link href="...">text</link>` or
/// `<link anchor="bookmark">text</link>`: the engine synthesizes a new
/// `OpaqueInline{Hyperlink}` at apply time and the serializer allocates
/// a fresh rId at export time.
///
/// Out of scope today: document-specific inline roles (`<defined_term>`).
/// Those still fail with `UnrecognizedTag`.
///
/// The `id` attribute on preserved-inline tags can be quoted with double
/// quotes, single quotes, or unquoted (for simple identifiers).
pub fn parse_paragraph_markup(text: &str) -> Result<ParagraphContent, MarkupParseError> {
    let mut fragments: Vec<ContentFragment> = Vec::new();
    let mut current_text = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut mark_stack: Vec<(&'static str, usize)> = Vec::new();
    let mut current_marks = InlineMarkSet::default();

    // Helper: flush `current_text` as a fragment, using StyledText when any
    // mark is active so downstream code can see which spans carry LLM marks.
    fn flush(
        fragments: &mut Vec<ContentFragment>,
        current_text: &mut String,
        current_marks: InlineMarkSet,
    ) {
        if current_text.is_empty() {
            return;
        }
        let text = std::mem::take(current_text);
        if current_marks.is_empty() {
            fragments.push(ContentFragment::Text(text));
        } else {
            fragments.push(ContentFragment::StyledText {
                text,
                marks: current_marks,
            });
        }
    }

    while i < chars.len() {
        if chars[i] == '<' {
            // Classify the `<`:
            //   `< ` / `<$` / `<5` etc. (whitespace/punct/digit) → literal
            //   `<foo` / `</foo` (ASCII letter or `/letter`) → parse as tag
            // This keeps everyday legal text like "amount < $1,000,000"
            // working while still catching malformed tag-like input.
            let next = chars.get(i + 1).copied();
            let tag_start = match next {
                Some(c) if c.is_ascii_alphabetic() => true,
                Some('/') => chars.get(i + 2).is_some_and(|c| c.is_ascii_alphabetic()),
                _ => false,
            };
            if !tag_start {
                current_text.push('<');
                i += 1;
                continue;
            }

            // Try preserved inline tag first (self-closing `<opaque .../>`
            // / `<anchor .../>`).
            if let Some(parsed) = try_parse_preserved_inline_tag(&chars, i)? {
                flush(&mut fragments, &mut current_text, current_marks);
                fragments.push(ContentFragment::PreservedInlineRef(NodeId::new(parsed.id)));
                i = parsed.end;
                continue;
            }

            // Try `<link href="..." [anchor="..."]>display text</link>`.
            // Note: mark tags inside a link body are not yet supported —
            // the link's display text is taken as plain text.
            if let Some(parsed) = try_parse_link_tag(&chars, i)? {
                flush(&mut fragments, &mut current_text, current_marks);
                fragments.push(ContentFragment::NewHyperlink {
                    href: parsed.href,
                    anchor: parsed.anchor,
                    text: parsed.text,
                });
                i = parsed.end;
                continue;
            }

            // Try universal mark tag (opening `<bold>` or closing `</bold>`).
            if let Some((tag_name, end, is_closing)) = try_parse_mark_tag(&chars, i)? {
                flush(&mut fragments, &mut current_text, current_marks);
                if is_closing {
                    // Must match the top of the mark stack.
                    let Some(&(top, _)) = mark_stack.last() else {
                        return Err(MarkupParseError::MismatchedCloseTag {
                            position: i,
                            expected: "<none>",
                            found: tag_name,
                        });
                    };
                    if top != tag_name {
                        return Err(MarkupParseError::MismatchedCloseTag {
                            position: i,
                            expected: top,
                            found: tag_name,
                        });
                    }
                    mark_stack.pop();
                    set_mark(&mut current_marks, tag_name, false);
                } else {
                    mark_stack.push((tag_name, i));
                    set_mark(&mut current_marks, tag_name, true);
                }
                i = end;
                continue;
            }

            // Looked like a tag but matched nothing we recognize.
            // Reject rather than silently treating as text.
            let snippet: String = chars[i..chars.len().min(i + 24)].iter().collect();
            return Err(MarkupParseError::UnrecognizedTag {
                position: i,
                snippet,
            });
        } else {
            current_text.push(chars[i]);
            i += 1;
        }
    }

    // Any unclosed mark tags are a hard error.
    if let Some(&(tag, position)) = mark_stack.last() {
        return Err(MarkupParseError::UnclosedMarkTag { tag, position });
    }

    // Flush remaining text.
    flush(&mut fragments, &mut current_text, current_marks);

    Ok(ParagraphContent { fragments })
}

/// Try to parse a universal-mark tag — either opening `<bold>` or closing
/// `</bold>`. Returns `(tag_name, end_index, is_closing)` when matched,
/// `None` otherwise. Mark tags carry no attributes in the MVP.
fn try_parse_mark_tag(
    chars: &[char],
    start: usize,
) -> Result<Option<(&'static str, usize, bool)>, MarkupParseError> {
    debug_assert_eq!(chars[start], '<');
    let after_lt = start + 1;
    if after_lt >= chars.len() {
        return Ok(None);
    }
    let is_closing = chars[after_lt] == '/';
    let name_start = if is_closing { after_lt + 1 } else { after_lt };
    // Read the tag name: ASCII letters only.
    let mut name_end = name_start;
    while name_end < chars.len() && chars[name_end].is_ascii_alphabetic() {
        name_end += 1;
    }
    if name_end == name_start {
        return Ok(None);
    }
    let name: String = chars[name_start..name_end].iter().collect();
    let Some(canonical) = match_mark_tag(&name) else {
        return Ok(None);
    };
    // Next char must be `>` (no attributes on mark tags in the MVP).
    if name_end >= chars.len() {
        return Err(MarkupParseError::UnterminatedTag { position: start });
    }
    if chars[name_end] != '>' {
        // Something like `<boldx>` — not our tag. Let the caller handle
        // the `<` as an unrecognized-tag error.
        return Ok(None);
    }
    Ok(Some((canonical, name_end + 1, is_closing)))
}

struct ParsedPreservedInlineTag {
    id: String,
    end: usize, // index after the closing `>`
}

/// Try to parse `<opaque id="..." />` or `<anchor id="..." />`.
/// Returns None if this isn't a preserved-inline tag.
/// Returns Err for malformed preserved-inline tags (opened but broken).
fn try_parse_preserved_inline_tag(
    chars: &[char],
    start: usize,
) -> Result<Option<ParsedPreservedInlineTag>, MarkupParseError> {
    let rest: String = chars[start..].iter().collect();

    let tag_name = if rest.starts_with("<opaque") {
        "opaque"
    } else if rest.starts_with("<anchor") {
        "anchor"
    } else {
        return Ok(None);
    };

    // After the tag name, must have whitespace or `/` or `>`.
    let after_tag = start + tag_name.len() + 1;
    if after_tag >= chars.len() {
        return Err(MarkupParseError::UnterminatedTag { position: start });
    }
    let next = chars[after_tag];
    if !next.is_whitespace() && next != '/' && next != '>' {
        return Ok(None); // e.g., `<anchored` — not our tag
    }

    // Find the closing `>`
    let close = chars[start..]
        .iter()
        .position(|&c| c == '>')
        .map(|p| start + p);
    let Some(close_pos) = close else {
        return Err(MarkupParseError::UnterminatedTag { position: start });
    };

    // Must be self-closing: character before `>` must be `/` (ignoring whitespace)
    let before_close: String = chars[after_tag..close_pos]
        .iter()
        .collect::<String>()
        .trim()
        .to_string();
    if !before_close.ends_with('/') {
        return Err(MarkupParseError::UnterminatedTag { position: start });
    }

    // Extract the attributes portion (between tag name and `/>`).
    let attrs_str = before_close.trim_end_matches('/').trim();

    // Parse `id` attribute
    let id = parse_id_attribute(attrs_str)
        .ok_or(MarkupParseError::OpaqueMissingId { position: start })?;

    Ok(Some(ParsedPreservedInlineTag {
        id,
        end: close_pos + 1,
    }))
}

struct ParsedLinkTag {
    href: Option<String>,
    anchor: Option<String>,
    text: String,
    end: usize, // index after the closing `</link>`
}

/// Try to parse `<link href="..." [anchor="..."]>display text</link>`
/// (or with `anchor` instead of `href` for internal cross-references).
/// Returns `None` if this isn't a link tag, `Err` if malformed.
///
/// The display text is parsed as plain text — nested mark/link/opaque
/// tags inside the link body are intentionally unsupported in the MVP
/// (the link itself is the formatting). The first `</link>` after the
/// opening tag terminates the body.
fn try_parse_link_tag(
    chars: &[char],
    start: usize,
) -> Result<Option<ParsedLinkTag>, MarkupParseError> {
    // Cheap prefix check.
    let prefix: String = chars[start..chars.len().min(start + 5)].iter().collect();
    if !prefix.starts_with("<link") {
        return Ok(None);
    }
    // After "<link" must be whitespace, `/`, or `>`. Otherwise it's a
    // different tag (e.g., `<linkage>`).
    let after_tag = start + 5;
    if after_tag >= chars.len() {
        return Err(MarkupParseError::UnterminatedTag { position: start });
    }
    let next = chars[after_tag];
    if !next.is_whitespace() && next != '/' && next != '>' {
        return Ok(None);
    }

    // Find the opening tag's closing `>`.
    let open_close = chars[after_tag..]
        .iter()
        .position(|&c| c == '>')
        .map(|p| after_tag + p)
        .ok_or(MarkupParseError::UnterminatedTag { position: start })?;

    // Self-closing `<link/>` makes no sense (no display text); reject.
    let attrs_segment: String = chars[after_tag..open_close].iter().collect();
    let attrs_str = attrs_segment.trim();
    if attrs_str.ends_with('/') {
        return Err(MarkupParseError::UnterminatedTag { position: start });
    }

    let href = parse_named_attribute(attrs_str, "href");
    let anchor = parse_named_attribute(attrs_str, "anchor");
    if href.is_none() && anchor.is_none() {
        // Both missing is a hard error — without a target the link
        // would silently render as plain text on accept. The "no
        // silent fallback" rule from CLAUDE.md.
        return Err(MarkupParseError::UnterminatedTag { position: start });
    }

    // Find the matching `</link>` from open_close+1.
    let body_start = open_close + 1;
    let close_seq = ['<', '/', 'l', 'i', 'n', 'k', '>'];
    let mut close_at: Option<usize> = None;
    let mut j = body_start;
    while j + close_seq.len() <= chars.len() {
        if chars[j..j + close_seq.len()] == close_seq {
            close_at = Some(j);
            break;
        }
        j += 1;
    }
    let Some(body_end) = close_at else {
        return Err(MarkupParseError::UnterminatedTag { position: start });
    };

    let text: String = chars[body_start..body_end].iter().collect();
    Ok(Some(ParsedLinkTag {
        href,
        anchor,
        text,
        end: body_end + close_seq.len(),
    }))
}

/// Extract the value of a named attribute from an attribute string.
/// Supports double-quoted, single-quoted, or unquoted forms. Returns
/// `None` when the attribute is absent.
fn parse_named_attribute(attrs: &str, name: &str) -> Option<String> {
    // Match `name` as a WHOLE attribute name, not a substring: it must be
    // preceded by whitespace (or start-of-string) and followed (after optional
    // whitespace) by `=`. A substring hit like `id` inside `data-oid="x"` is
    // skipped, and scanning continues to a real later occurrence.
    let mut from = 0;
    while let Some(rel) = attrs[from..].find(name) {
        let pos = from + rel;
        from = pos + name.len();

        if pos > 0 && !attrs.as_bytes()[pos - 1].is_ascii_whitespace() {
            continue; // substring of a longer attribute name
        }
        let after = attrs[pos + name.len()..].trim_start();
        let Some(after_eq) = after.strip_prefix('=') else {
            continue; // a bare/boolean attribute named `name`, not `name=...`
        };
        let value_str = after_eq.trim_start();
        return if let Some(rest) = value_str.strip_prefix('"') {
            rest.find('"').map(|end| rest[..end].to_string())
        } else if let Some(rest) = value_str.strip_prefix('\'') {
            rest.find('\'').map(|end| rest[..end].to_string())
        } else {
            let end = value_str
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(value_str.len());
            if end == 0 {
                None
            } else {
                Some(value_str[..end].to_string())
            }
        };
    }
    None
}

/// Extract the `id` value from an attribute string like `id="foo"` or `id='foo'`
/// or `id=foo`. Shares the whole-name matching of `parse_named_attribute` so a
/// substring like `id` inside `data-oid` is never misread as the `id` attribute.
fn parse_id_attribute(attrs: &str) -> Option<String> {
    parse_named_attribute(attrs, "id")
}

// ─── Unit tests ────────────────────────────────────────────────────────────
//
// Pure string-in / Result-out contracts for the legacy markup parser. Each
// malformed input must produce a TYPED `MarkupParseError` (no silent fallback
// to "treat as text"), and well-formed input must produce the expected
// fragments.
#[cfg(test)]
mod tests {
    use super::*;

    // ── error variants ───────────────────────────────────────────────────────

    /// A close tag that doesn't match the currently-open mark is a hard error,
    /// not a best-effort recovery: `<bold><italic>x</bold>` closes bold while
    /// italic is on top of the stack.
    #[test]
    fn mismatched_close_tag_is_rejected() {
        let err = parse_paragraph_markup("<bold><italic>x</bold></italic>")
            .expect_err("crossed tags must fail");
        match err {
            MarkupParseError::MismatchedCloseTag {
                expected, found, ..
            } => {
                assert_eq!(expected, "italic", "the open mark on top of the stack");
                assert_eq!(found, "bold", "the tag that tried to close out of order");
            }
            other => panic!("expected MismatchedCloseTag, got {other:?}"),
        }
    }

    /// A closing tag with no open mark at all is also a MismatchedCloseTag
    /// (expected "<none>").
    #[test]
    fn close_tag_with_empty_stack_is_rejected() {
        let err = parse_paragraph_markup("plain</bold>").expect_err("stray close tag must fail");
        match err {
            MarkupParseError::MismatchedCloseTag {
                expected, found, ..
            } => {
                assert_eq!(expected, "<none>");
                assert_eq!(found, "bold");
            }
            other => panic!("expected MismatchedCloseTag, got {other:?}"),
        }
    }

    /// An opening mark tag with no matching close is a hard error.
    #[test]
    fn unclosed_mark_tag_is_rejected() {
        let err =
            parse_paragraph_markup("<bold>dangling").expect_err("unclosed mark tag must fail");
        match err {
            MarkupParseError::UnclosedMarkTag { tag, .. } => assert_eq!(tag, "bold"),
            other => panic!("expected UnclosedMarkTag, got {other:?}"),
        }
    }

    /// An `<opaque>` tag without an `id` attribute cannot be resolved to a
    /// preserved inline, so it is rejected rather than silently dropped.
    #[test]
    fn opaque_without_id_is_rejected() {
        let err =
            parse_paragraph_markup("before <opaque /> after").expect_err("missing id must fail");
        assert!(
            matches!(err, MarkupParseError::OpaqueMissingId { .. }),
            "expected OpaqueMissingId, got {err:?}"
        );
    }

    /// An attribute whose name merely CONTAINS `id` as a substring (e.g.
    /// `data-oid`) must not be misread as the `id` attribute. With no real
    /// `id`, the `<opaque>` must fail loud rather than bind to a garbage
    /// NodeId (CLAUDE.md: no silent fallback — silently resolving an opaque to
    /// the wrong node is exactly the "hide uncertainty behind a catch-all" bug).
    #[test]
    fn opaque_with_only_a_substring_id_attribute_is_rejected() {
        let err = parse_paragraph_markup(r#"<opaque data-oid="x"/>"#)
            .expect_err("substring-only id must not resolve");
        assert!(
            matches!(err, MarkupParseError::OpaqueMissingId { .. }),
            "expected OpaqueMissingId, got {err:?}"
        );
    }

    /// Attribute matching is by whole name, not substring: `id` inside
    /// `data-oid` is skipped, and a real `id` appearing AFTER it is still found.
    #[test]
    fn named_attribute_matches_whole_name_not_substring() {
        assert_eq!(parse_named_attribute(r#"data-oid="x""#, "id"), None);
        assert_eq!(
            parse_named_attribute(r#"data-oid="zzz" id="real""#, "id"),
            Some("real".to_string())
        );
        assert_eq!(
            parse_named_attribute(r#"id="ok""#, "id"),
            Some("ok".to_string())
        );
        // single-quoted / unquoted forms still work
        assert_eq!(parse_id_attribute(r#"id='q'"#), Some("q".to_string()));
        assert_eq!(parse_id_attribute(r#"id=u"#), Some("u".to_string()));
    }

    /// An `<opaque` tag opened but never self-closed (`/>`) is unterminated.
    #[test]
    fn unterminated_opaque_tag_is_rejected() {
        let err = parse_paragraph_markup(r#"<opaque id="f1""#)
            .expect_err("unterminated opaque tag must fail");
        assert!(
            matches!(err, MarkupParseError::UnterminatedTag { .. }),
            "expected UnterminatedTag, got {err:?}"
        );
    }

    // ── happy path ───────────────────────────────────────────────────────────

    /// Plain text, a bold span, and a preserved-inline reference all parse into
    /// the expected fragments with the mark union carried on the styled span.
    #[test]
    fn parses_text_marks_and_opaque() {
        let content = parse_paragraph_markup(r#"Hello <bold>world</bold> <opaque id="f1"/>"#)
            .expect("well-formed markup parses");
        let frags = &content.fragments;
        assert_eq!(frags.len(), 4, "text / styled / text / opaque: {frags:?}");

        match &frags[0] {
            ContentFragment::Text(t) => assert_eq!(t, "Hello "),
            other => panic!("expected leading Text, got {other:?}"),
        }
        match &frags[1] {
            ContentFragment::StyledText { text, marks } => {
                assert_eq!(text, "world");
                assert!(marks.bold, "the bold mark must be set on the span");
                assert!(!marks.italic, "only bold should be set");
            }
            other => panic!("expected StyledText, got {other:?}"),
        }
        match &frags[2] {
            ContentFragment::Text(t) => assert_eq!(t, " "),
            other => panic!("expected separating Text, got {other:?}"),
        }
        match &frags[3] {
            ContentFragment::PreservedInlineRef(id) => assert_eq!(id, &NodeId::from("f1")),
            other => panic!("expected PreservedInlineRef, got {other:?}"),
        }
    }

    /// Nested, balanced marks compose: inside `<bold><italic>…` both marks are
    /// active on the span.
    #[test]
    fn nested_balanced_marks_compose() {
        let content = parse_paragraph_markup("<bold><italic>both</italic></bold>")
            .expect("balanced nesting parses");
        assert_eq!(content.fragments.len(), 1);
        match &content.fragments[0] {
            ContentFragment::StyledText { text, marks } => {
                assert_eq!(text, "both");
                assert!(marks.bold && marks.italic, "both marks active: {marks:?}");
            }
            other => panic!("expected StyledText, got {other:?}"),
        }
    }
}
