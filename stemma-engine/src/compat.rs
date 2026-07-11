//! Import-edge tolerance for schema-INVALID but Word-ACCEPTED input shapes.
//!
//! Stemma's importer is fail-fast: an XML shape the WordprocessingML schema
//! does not allow is refused rather than best-effort-parsed (no silent
//! fallbacks). This module is the single, named exception boundary to that
//! rule. It is NOT a fallback — it is a *closed, enumerated* set of shapes that
//! are schema-invalid yet which Microsoft Word itself opens WITHOUT repair and
//! renders deterministically. Each has a real-world witness Word accepts, so we
//! honor Word's behavior as a contractual default: the invalid shape is
//! rewritten into the equivalent valid shape here, at the import edge, so the
//! core model never sees it — and every rewrite records an import [`Diagnostic`]
//! so the tolerance is *observable*, never silent.
//!
//! Anything outside this enumerated set still fails fast at the normal parse
//! sites. Adding a shape here requires a witness that Word opens `{valid,
//! repaired:false}`.
//!
//! Tolerated shapes:
//!  1. `<w:shd w:val="none"/>` — `ST_Shd` (ISO/IEC 29500-1 §17.18.78) has no
//!     `none` member. Word renders it as no shading. We treat `none` as "no
//!     shading pattern": the `w:val` is dropped, and a `w:shd` left with no
//!     attributes (the common case: `<w:shd w:val="none"/>` carries no fill or
//!     color) is removed entirely. A `w:shd` that still names a fill/color keeps
//!     those (pattern-less fill = what Word renders). Applies uniformly wherever
//!     `w:shd` occurs (pPr, rPr, tcPr).
//!  2. nested `<w:r>` inside `<w:r>` — `CT_R` (ISO/IEC 29500-1 §17.3.2.25) has
//!     no `r` child; some generators emit it inside TOC/field runs. Word treats
//!     the inner run as content. We flatten it into sibling runs, each keeping
//!     its OWN run properties, with content order preserved.
//!
//! A third shape — `<w:tbl>` as a direct child of `<w:p>` (`CT_P`,
//! §17.3.1.22, has no `tbl` child) — is tolerated too, but its rewrite is
//! structural: the table becomes a new BLOCK-LEVEL sibling. That hoist lives
//! next to block assembly in `import::append_blocks_from_element`; this module
//! only provides the paragraph/table split it uses ([`split_paragraph_tables`]).

use xmltree::{Element, XMLNode};

use crate::runtime::{Diagnostic, DiagnosticLevel};
use crate::word_xml::{is_w_tag, w_el};
use crate::xml_attrs::attr_get;

/// A `w:shd` whose `w:val` is the schema-invalid, Word-tolerated `"none"`.
fn is_shd_val_none(el: &Element) -> bool {
    is_w_tag(el, "shd") && attr_get(el, "w:val").map(|v| v == "none").unwrap_or(false)
}

/// A `w:r` (run).
fn is_run(el: &Element) -> bool {
    is_w_tag(el, "r")
}

/// A run that directly contains another run — the schema-invalid nesting.
fn has_nested_run(run: &Element) -> bool {
    run.children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(e) if is_run(e)))
}

/// True when `element`'s subtree contains any shape [`normalize_tolerated_shapes`]
/// rewrites. A cheap, read-only pre-check so a fully conformant subtree is never
/// cloned (mirrors the MCE preprocessor's `needs_transform` gate).
pub(crate) fn subtree_has_tolerated_shape(element: &Element) -> bool {
    if is_shd_val_none(element) {
        return true;
    }
    if is_run(element) && has_nested_run(element) {
        return true;
    }
    element
        .children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(e) if subtree_has_tolerated_shape(e)))
}

fn shd_none_diagnostic() -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Info,
        message: "tolerated schema-invalid <w:shd w:val=\"none\">: ST_Shd \
                  (ISO/IEC 29500-1 §17.18.78) has no \"none\" member, but Word \
                  opens such documents without repair and renders no shading — \
                  treated as no shading pattern"
            .to_string(),
        context: Some("w:shd".to_string()),
    }
}

fn nested_run_diagnostic() -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Warning,
        message: "tolerated schema-invalid nested <w:r> inside <w:r>: CT_R \
                  (ISO/IEC 29500-1 §17.3.2.25) has no r child, but Word opens \
                  such documents without repair and renders the inner run as \
                  content — flattened into sibling runs, each keeping its own \
                  run properties, content order preserved"
            .to_string(),
        context: Some("w:r".to_string()),
    }
}

/// The diagnostic recorded when a `w:tbl` direct child of a `w:p` is hoisted to
/// a block-level sibling. Lives here (not at the import call site) so all three
/// tolerance messages read from one place. `body_index` locates the host block.
pub(crate) fn tbl_in_paragraph_diagnostic(context: String) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Warning,
        message: "tolerated schema-invalid <w:tbl> as a direct child of <w:p>: \
                  CT_P (ISO/IEC 29500-1 §17.3.1.22) has no tbl child, but Word \
                  opens such documents without repair and renders the table as \
                  block content — hoisted to a block-level sibling immediately \
                  after the paragraph (roundtrip differs from the invalid \
                  original by design)"
            .to_string(),
        context: Some(context),
    }
}

/// Rewrite the two within-subtree tolerated shapes (`w:shd w:val="none"` and
/// nested `w:r`), pushing one [`Diagnostic`] per rewrite. Returns an owned,
/// rewritten clone of `element`. Callers gate on
/// [`subtree_has_tolerated_shape`], so this only clones when a rewrite is
/// actually needed.
pub(crate) fn normalize_tolerated_shapes(
    element: &Element,
    diagnostics: &mut Vec<Diagnostic>,
) -> Element {
    let mut out = element.clone();
    out.children = rewrite_children(&element.children, diagnostics);
    out
}

/// Rebuild a children list, applying both within-subtree tolerances:
/// - a `w:r` holding a nested `w:r` is replaced by its flattened sibling runs;
/// - a `w:shd w:val="none"` has its `val` dropped (and, if left empty, the whole
///   element removed);
/// - every other element is rewritten recursively so the tolerances reach any
///   depth (rPr/tcPr shading, runs inside hyperlinks/ins/del, ...).
fn rewrite_children(children: &[XMLNode], diagnostics: &mut Vec<Diagnostic>) -> Vec<XMLNode> {
    let mut result = Vec::with_capacity(children.len());
    for node in children {
        let element = match node {
            XMLNode::Element(e) => e,
            other => {
                result.push(other.clone());
                continue;
            }
        };

        if is_run(element) && has_nested_run(element) {
            diagnostics.push(nested_run_diagnostic());
            for run in flatten_run(element, diagnostics) {
                result.push(XMLNode::Element(run));
            }
        } else if is_shd_val_none(element) {
            diagnostics.push(shd_none_diagnostic());
            if let Some(stripped) = strip_shd_val(element) {
                result.push(XMLNode::Element(stripped));
            }
        } else {
            result.push(XMLNode::Element(normalize_tolerated_shapes(
                element,
                diagnostics,
            )));
        }
    }
    result
}

/// Drop the `w:val` attribute from a `w:shd`. Returns `None` when that leaves an
/// attribute-less, childless `w:shd` (nothing left to represent — i.e. "no
/// shading"), so the caller removes the element entirely; `Some` when a fill or
/// color remains.
fn strip_shd_val(shd: &Element) -> Option<Element> {
    let mut out = shd.clone();
    let val_keys: Vec<_> = out
        .attributes
        .keys()
        .filter(|k| k.local_name == "val")
        .cloned()
        .collect();
    for key in val_keys {
        out.attributes.shift_remove(&key);
    }
    let has_child_element = out
        .children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(_)));
    if out.attributes.is_empty() && !has_child_element {
        None
    } else {
        Some(out)
    }
}

/// Flatten a run that directly contains one or more nested runs into a sequence
/// of sibling runs, in document order. Content between/around nested runs is
/// wrapped in runs that carry the OUTER run's `rPr`; each nested run is emitted
/// as its own sibling keeping its OWN `rPr`. Recurses so multiply-nested runs
/// flatten fully. A run with no nested run is returned as a single
/// (recursively normalized) run.
fn flatten_run(run: &Element, diagnostics: &mut Vec<Diagnostic>) -> Vec<Element> {
    let outer_rpr = run.children.iter().find_map(|c| match c {
        XMLNode::Element(e) if is_w_tag(e, "rPr") => Some(e.clone()),
        _ => None,
    });

    let mut result: Vec<Element> = Vec::new();
    let mut pending: Vec<XMLNode> = Vec::new();

    for node in &run.children {
        match node {
            XMLNode::Element(e) if is_w_tag(e, "rPr") => {
                // Captured as `outer_rpr`; not content.
            }
            XMLNode::Element(e) if is_run(e) => {
                flush_pending(&mut result, &mut pending, &outer_rpr);
                if has_nested_run(e) {
                    diagnostics.push(nested_run_diagnostic());
                    result.extend(flatten_run(e, diagnostics));
                } else {
                    // Inner run keeps its own rPr; normalize its content
                    // (e.g. an rPr shd=none) via the standard recursion.
                    result.push(normalize_tolerated_shapes(e, diagnostics));
                }
            }
            XMLNode::Element(e) => {
                pending.push(XMLNode::Element(normalize_tolerated_shapes(e, diagnostics)));
            }
            other => pending.push(other.clone()),
        }
    }
    flush_pending(&mut result, &mut pending, &outer_rpr);

    if result.is_empty() {
        // Defensive: a run detected as nested must have produced siblings; if
        // not (e.g. only an rPr), emit a single normalized run so no content is
        // dropped.
        result.push(make_run(&outer_rpr, Vec::new()));
    }
    result
}

/// Emit the buffered non-run content (if any) as one run carrying `rpr`.
fn flush_pending(result: &mut Vec<Element>, pending: &mut Vec<XMLNode>, rpr: &Option<Element>) {
    if pending.is_empty() {
        return;
    }
    result.push(make_run(rpr, std::mem::take(pending)));
}

/// Build a `w:r` with an optional leading `rPr` followed by `content`.
fn make_run(rpr: &Option<Element>, content: Vec<XMLNode>) -> Element {
    let mut run = w_el("r");
    if let Some(rpr) = rpr {
        run.children.push(XMLNode::Element(rpr.clone()));
    }
    run.children.extend(content);
    run
}

/// Split a `w:p` carrying direct-child `w:tbl` elements into the paragraph with
/// its tables removed (remaining children preserved in document order) plus the
/// removed tables in order. The importer hoists each table to a block-level
/// sibling after the paragraph. See [`tbl_in_paragraph_diagnostic`].
pub(crate) fn split_paragraph_tables(paragraph: &Element) -> (Element, Vec<Element>) {
    let mut tables = Vec::new();
    let mut kept = Vec::new();
    for node in &paragraph.children {
        match node {
            XMLNode::Element(e) if is_w_tag(e, "tbl") => tables.push(e.clone()),
            other => kept.push(other.clone()),
        }
    }
    let mut p = paragraph.clone();
    p.children = kept;
    (p, tables)
}

/// True when `paragraph` has a `w:tbl` as a direct child (the schema-invalid
/// shape [`split_paragraph_tables`] handles).
pub(crate) fn paragraph_has_direct_table(paragraph: &Element) -> bool {
    paragraph
        .children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(e) if is_w_tag(e, "tbl")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(xml: &str) -> Element {
        Element::parse(Cursor::new(xml.as_bytes())).expect("parse test XML")
    }

    fn local_names(children: &[XMLNode]) -> Vec<String> {
        children
            .iter()
            .filter_map(|c| match c {
                XMLNode::Element(e) => Some(e.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn detects_only_tolerated_shapes() {
        let conformant = parse(
            r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                 <w:pPr><w:shd w:val="clear" w:fill="FF0000"/></w:pPr>
                 <w:r><w:t>ok</w:t></w:r></w:p>"#,
        );
        assert!(!subtree_has_tolerated_shape(&conformant));

        let shd_none = parse(
            r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                 <w:pPr><w:shd w:val="none"/></w:pPr></w:p>"#,
        );
        assert!(subtree_has_tolerated_shape(&shd_none));

        let nested = parse(
            r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                 <w:r><w:r><w:t>x</w:t></w:r></w:r></w:p>"#,
        );
        assert!(subtree_has_tolerated_shape(&nested));
    }

    #[test]
    fn empty_shd_none_is_dropped_but_fill_bearing_shd_is_kept() {
        let mut diags = Vec::new();
        // val="none" only → element removed entirely.
        let ppr = parse(
            r#"<w:pPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                 <w:shd w:val="none"/></w:pPr>"#,
        );
        let out = normalize_tolerated_shapes(&ppr, &mut diags);
        assert!(
            local_names(&out.children).is_empty(),
            "empty w:shd must be removed"
        );
        // val="none" with a fill → val stripped, element (and fill) kept.
        let ppr2 = parse(
            r#"<w:pPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                 <w:shd w:val="none" w:fill="FF0000"/></w:pPr>"#,
        );
        let out2 = normalize_tolerated_shapes(&ppr2, &mut diags);
        let shd = out2
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(e) if is_w_tag(e, "shd") => Some(e),
                _ => None,
            })
            .expect("fill-bearing w:shd must be kept");
        assert!(
            attr_get(shd, "w:val").is_none(),
            "val=none must be stripped"
        );
        assert_eq!(attr_get(shd, "w:fill").map(String::as_str), Some("FF0000"));
    }

    #[test]
    fn split_paragraph_tables_preserves_order_and_extracts_all_tables() {
        let p = parse(
            r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                 <w:r><w:t>a</w:t></w:r>
                 <w:tbl><w:tr/></w:tbl>
                 <w:r><w:t>b</w:t></w:r>
                 <w:tbl><w:tr/></w:tbl></w:p>"#,
        );
        assert!(paragraph_has_direct_table(&p));
        let (kept, tables) = split_paragraph_tables(&p);
        assert_eq!(local_names(&kept.children), vec!["r", "r"]);
        assert_eq!(tables.len(), 2);
    }

    #[test]
    fn flatten_run_promotes_inner_run_and_keeps_its_rpr() {
        let mut diags = Vec::new();
        let p = parse(
            r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                 <w:r><w:rPr><w:i/></w:rPr>
                   <w:fldChar w:fldCharType="begin"/>
                   <w:r><w:rPr><w:b/></w:rPr><w:t>inner</w:t></w:r>
                   <w:fldChar w:fldCharType="end"/>
                 </w:r></w:p>"#,
        );
        let out = normalize_tolerated_shapes(&p, &mut diags);
        // The single outer run becomes three sibling runs: [begin][inner][end].
        assert_eq!(local_names(&out.children), vec!["r", "r", "r"]);
        assert_eq!(diags.len(), 1);
        // Inner run (the middle sibling) keeps its own bold rPr and its text.
        let inner = match &out.children[1] {
            XMLNode::Element(e) => e,
            _ => panic!("run element"),
        };
        assert!(
            inner
                .children
                .iter()
                .any(|c| matches!(c, XMLNode::Element(e) if is_w_tag(e, "rPr")
            && e.children.iter().any(|g| matches!(g, XMLNode::Element(ge) if is_w_tag(ge, "b")))))
        );
    }
}
