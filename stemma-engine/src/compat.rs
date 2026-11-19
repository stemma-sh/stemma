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
//!  5. a `w:del` or `w:ins` inside a `w:r` — `CT_R` has no such child.
//!     Checked against Word (accept, reject and plain resave): it reads the
//!     run's content up to the change as ordinary text and the change's
//!     content as that change on a run with the outer run's properties, and
//!     writes `<w:del><w:r>…</w:r></w:del>` on save. The run is split that
//!     way. Only the confirmed shape is rewritten: the change is the run's
//!     last child and holds run content only (no run, no nested change, and
//!     for a deletion no `w:t`). Content after the change in the same run is
//!     read by Word as part of the change too; that is not rewritten, and the
//!     import refuses it as before.
//!
//! Two more shapes are schema-VALID but carry tracked numbering the model
//! spells another way; Word's reading of each is verified on the oracle and
//! the rewrite reproduces it (see `rewrite_tracked_numbering`):
//!  3. `w:numPr/w:numberingChange` (legacy previous numbering
//!     text). Word lists no revision for it and drops it on Accept and on
//!     Reject alike, the numbering staying. It is not modeled (an untouched
//!     paragraph is still written as it was read).
//!  4. `w:numPr/w:ins` (inserted numbering). Word lists one
//!     revision and keeps the numbering on Accept AND on Reject, dropping only
//!     the marker. That is a tracked paragraph-property change whose previous
//!     properties equal the current ones: it becomes a `w:pPrChange` carrying
//!     the marker's id, author and date. A paragraph that already has a
//!     `w:pPrChange` keeps the marker, and the import refuses it as before:
//!     two revisions cannot become one.
//!
//! A further shape — `<w:tbl>` as a direct child of `<w:p>` (`CT_P`,
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
    if is_w_tag(element, "p") && paragraph_num_pr(element).is_some_and(has_tracked_numbering) {
        return true;
    }
    if is_run(element) && has_nested_run(element) {
        return true;
    }
    if is_run(element) && tracked_change_in_run(element).is_some() {
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
    if is_w_tag(&out, "p") {
        rewrite_tracked_numbering(&mut out, diagnostics);
    }
    out
}

/// The live `w:pPr/w:numPr` of a paragraph (never the previous one inside a
/// `w:pPrChange`).
fn paragraph_num_pr(paragraph: &Element) -> Option<&Element> {
    find_child(paragraph, "pPr").and_then(|ppr| find_child(ppr, "numPr"))
}

fn find_child<'a>(element: &'a Element, local: &str) -> Option<&'a Element> {
    element.children.iter().find_map(|child| match child {
        XMLNode::Element(e) if is_w_tag(e, local) => Some(e),
        _ => None,
    })
}

fn has_tracked_numbering(num_pr: &Element) -> bool {
    num_pr.children.iter().any(|child| {
        matches!(child, XMLNode::Element(e) if is_w_tag(e, "ins") || is_w_tag(e, "numberingChange"))
    })
}

/// Shapes 3 and 4: rewrite a paragraph's tracked numbering children as Word
/// reads them, one diagnostic per rewrite.
fn rewrite_tracked_numbering(paragraph: &mut Element, diagnostics: &mut Vec<Diagnostic>) {
    let Some(ppr) = paragraph.children.iter_mut().find_map(|child| match child {
        XMLNode::Element(e) if is_w_tag(e, "pPr") => Some(e),
        _ => None,
    }) else {
        return;
    };
    let has_ppr_change = find_child(ppr, "pPrChange").is_some();
    let Some(num_pr) = ppr.children.iter_mut().find_map(|child| match child {
        XMLNode::Element(e) if is_w_tag(e, "numPr") => Some(e),
        _ => None,
    }) else {
        return;
    };
    let before = num_pr.children.len();
    num_pr
        .children
        .retain(|child| !matches!(child, XMLNode::Element(e) if is_w_tag(e, "numberingChange")));
    if num_pr.children.len() != before {
        diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Info,
            message: "w:numPr/w:numberingChange (legacy previous numbering text) is not \
                      modeled: Word lists no revision for it and drops it on Accept and on \
                      Reject, keeping the numbering; an untouched paragraph keeps it as written"
                .to_string(),
            context: Some("w:numberingChange".to_string()),
        });
    }
    let Some(index) = num_pr
        .children
        .iter()
        .position(|child| matches!(child, XMLNode::Element(e) if is_w_tag(e, "ins")))
    else {
        return;
    };
    // A marker in a numPr that names no list (no w:numId, no w:ilvl): Word
    // lists no revision for it and keeps it, inert, through Accept All,
    // Reject All and a plain save (observed). The model drops it.
    let marker_only = !num_pr.children.iter().any(
        |child| matches!(child, XMLNode::Element(e) if is_w_tag(e, "numId") || is_w_tag(e, "ilvl")),
    );
    if marker_only {
        num_pr.children.remove(index);
        diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Info,
            message: "w:numPr/w:ins in a numPr that names no list is not modeled: Word lists \
                      no revision for it and keeps it inert on Accept, Reject and save"
                .to_string(),
            context: Some("w:numPr/w:ins".to_string()),
        });
        return;
    }
    if has_ppr_change {
        // With a w:pPrChange beside it, Word lists the marker as its own
        // paragraph-number revision; Accept keeps the numbering and the
        // current properties, Reject restores the previous properties, with
        // their numbering or without it (observed, the previous properties
        // naming the same list, and naming none). Dropping the marker gives
        // the same Accept and Reject results; the model then lists one
        // revision fewer than Word.
        num_pr.children.remove(index);
        diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Info,
            message: "w:numPr/w:ins beside a w:pPrChange is not modeled as its own revision: \
                      Word lists it separately, but Accept and Reject give the same \
                      paragraphs without it"
                .to_string(),
            context: Some("w:numPr/w:ins".to_string()),
        });
        return;
    }
    let XMLNode::Element(ins) = num_pr.children.remove(index) else {
        unreachable!("position found an element");
    };
    let mut previous = w_el("pPr");
    previous.children = ppr
        .children
        .iter()
        .filter(|child| {
            !matches!(child, XMLNode::Element(e)
                if is_w_tag(e, "rPr") || is_w_tag(e, "sectPr") || is_w_tag(e, "pPrChange"))
        })
        .cloned()
        .collect();
    let mut change = w_el("pPrChange");
    change.attributes = ins.attributes.clone();
    change.children = vec![XMLNode::Element(previous)];
    ppr.children.push(XMLNode::Element(change));
    diagnostics.push(Diagnostic {
        level: DiagnosticLevel::Info,
        message: "read w:numPr/w:ins (inserted numbering) as \
                  Word does: one revision whose Accept and Reject both keep the numbering, \
                  written back as a w:pPrChange with the same previous properties"
            .to_string(),
        context: Some("w:numPr/w:ins".to_string()),
    });
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

        if is_run(element)
            && let Some(change) = tracked_change_in_run(element)
        {
            diagnostics.push(tracked_change_in_run_diagnostic(&change.name));
            result.extend(
                lift_tracked_change(element, change)
                    .into_iter()
                    .map(XMLNode::Element),
            );
        } else if is_run(element) && has_nested_run(element) {
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

/// The `w:del` or `w:ins` a run holds as its last child, when the run has the
/// shape [`lift_tracked_change`] rewrites: exactly one such child, nothing
/// after it, and inside it only run content — no `w:r`, no nested change,
/// and for a deletion no `w:t` (Word repairs that; it is left for validation
/// to report).
fn tracked_change_in_run(run: &Element) -> Option<&Element> {
    let children: Vec<&Element> = run
        .children
        .iter()
        .filter_map(|child| match child {
            XMLNode::Element(element) => Some(element),
            _ => None,
        })
        .collect();
    let is_change = |element: &Element| is_w_tag(element, "del") || is_w_tag(element, "ins");
    let changes: Vec<&&Element> = children.iter().filter(|child| is_change(child)).collect();
    let [change] = changes.as_slice() else {
        return None;
    };
    if !std::ptr::eq(**change, *children.last()?) {
        return None;
    }
    let is_deletion = is_w_tag(change, "del");
    let holds_run_content_only = change.children.iter().all(|child| match child {
        XMLNode::Element(inner) => {
            !is_run(inner)
                && !is_change(inner)
                && !is_w_tag(inner, "moveFrom")
                && !is_w_tag(inner, "moveTo")
                && !is_w_tag(inner, "rPr")
                && !(is_deletion && is_w_tag(inner, "t"))
        }
        _ => true,
    });
    holds_run_content_only.then_some(**change)
}

/// Split a run holding a tracked change (see [`tracked_change_in_run`]):
/// the content before the change stays a run with the run's properties, and
/// the change's content becomes a run with the same properties inside a copy
/// of the change element.
fn lift_tracked_change(run: &Element, change: &Element) -> Vec<Element> {
    let rpr = run.children.iter().find_map(|child| match child {
        XMLNode::Element(element) if is_w_tag(element, "rPr") => Some(element.clone()),
        _ => None,
    });
    let before: Vec<XMLNode> = run
        .children
        .iter()
        .filter(|child| match child {
            XMLNode::Element(element) => {
                !is_w_tag(element, "rPr") && !std::ptr::eq(element, change)
            }
            _ => false,
        })
        .cloned()
        .collect();
    let mut out = Vec::new();
    if !before.is_empty() {
        let mut head = run.clone();
        head.children = rpr.iter().cloned().map(XMLNode::Element).collect();
        head.children.extend(before);
        out.push(head);
    }
    let mut inner = run.clone();
    inner.children = rpr.iter().cloned().map(XMLNode::Element).collect();
    inner.children.extend(change.children.iter().cloned());
    let mut wrapper = change.clone();
    wrapper.children = vec![XMLNode::Element(inner)];
    out.push(wrapper);
    out
}

fn tracked_change_in_run_diagnostic(change: &str) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Warning,
        message: format!(
            "tolerated schema-invalid <{change}> inside <w:r>: CT_R (ISO/IEC 29500-1 \
             §17.3.2.25) has no such child, but Word opens such documents without repair \
             and reads the change's content as that change on a run with the outer run's \
             properties — rewritten that way"
        ),
        context: Some("w:r".to_string()),
    }
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

/// Zero-width markers that may follow the run holding paragraphs (see
/// [`split_paragraphs_in_text`]) or precede it.
const MARKERS: [&str; 3] = ["bookmarkStart", "bookmarkEnd", "proofErr"];

fn element_children(element: &Element) -> impl Iterator<Item = &Element> {
    element.children.iter().filter_map(|node| match node {
        XMLNode::Element(element) => Some(element),
        _ => None,
    })
}

/// The run of `paragraph` whose `w:t` holds paragraphs, with its index among
/// the paragraph's children, when the paragraph has the shape
/// [`split_paragraphs_in_text`] reads.
fn run_holding_paragraphs(paragraph: &Element) -> Option<(usize, &Element)> {
    let holds = |run: &Element| {
        element_children(run)
            .any(|t| is_w_tag(t, "t") && element_children(t).any(|p| is_w_tag(p, "p")))
    };
    let (index, run) =
        paragraph
            .children
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                XMLNode::Element(run) if is_run(run) && holds(run) => Some((index, run)),
                _ => None,
            })?;
    // The run holds its properties and the one w:t.
    let run_children: Vec<&Element> = element_children(run).collect();
    let [.., text] = run_children.as_slice() else {
        return None;
    };
    if run_children.len() > 2 || (run_children.len() == 2 && !is_w_tag(run_children[0], "rPr")) {
        return None;
    }
    // The w:t holds optional text, then paragraphs only, and no text after
    // the first paragraph.
    let mut seen_paragraph = false;
    for node in &text.children {
        match node {
            XMLNode::Element(inner) if is_w_tag(inner, "p") => seen_paragraph = true,
            XMLNode::Element(_) => return None,
            XMLNode::Text(t) | XMLNode::CData(t) if seen_paragraph && !t.trim().is_empty() => {
                return None;
            }
            _ => {}
        }
    }
    // Before the run: properties, runs and markers; after it: markers only.
    let fits = |element: &Element, after: bool| {
        MARKERS.iter().any(|marker| is_w_tag(element, marker))
            || (!after && (is_w_tag(element, "pPr") || is_run(element)))
    };
    for (position, node) in paragraph.children.iter().enumerate() {
        if let XMLNode::Element(element) = node
            && position != index
            && !fits(element, position > index)
        {
            return None;
        }
    }
    Some((index, run))
}

/// Whether a paragraph has a run whose `w:t` holds paragraphs, in the shape
/// [`split_paragraphs_in_text`] reads.
pub(crate) fn paragraph_has_paragraphs_in_text(paragraph: &Element) -> bool {
    run_holding_paragraphs(paragraph).is_some()
}

/// Read a paragraph whose run holds paragraphs inside its `w:t` the way Word
/// does. `w:t` holds text only, but Word opens such a paragraph without
/// repair and shows the paragraphs inside (observed, Word 16.0, by plain
/// resave):
///
/// - the first inner paragraph takes the outer paragraph's content before
///   the run and the w:t's own leading text (a run with the outer run's
///   properties), then its own runs. Its properties are its own, or, when it
///   has none, the outer paragraph's; its paragraph-mark run properties are
///   the outer run's, overlaid by its own (its own win where both state one);
/// - the other inner paragraphs follow as they are;
/// - the outer paragraph remains, with its own properties and the markers
///   that followed the run, and no text.
///
/// Read only when the w:t holds nothing after its first paragraph but
/// paragraphs, the run holds only its properties and that w:t, and the outer
/// paragraph holds only runs and markers before it and markers after it.
/// Word cannot open text or a run after the inner paragraphs (reported by
/// validation); other shapes are not read.
pub(crate) fn split_paragraphs_in_text(paragraph: &Element) -> Option<Vec<Element>> {
    let (index, run) = run_holding_paragraphs(paragraph)?;
    let run_properties = element_children(run).find(|child| is_w_tag(child, "rPr"));
    let text = element_children(run).find(|child| is_w_tag(child, "t"))?;
    let outer_properties = element_children(paragraph).find(|child| is_w_tag(child, "pPr"));

    let leading_text: String = text
        .children
        .iter()
        .take_while(|node| !matches!(node, XMLNode::Element(_)))
        .filter_map(|node| match node {
            XMLNode::Text(t) | XMLNode::CData(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let mut inner = element_children(text).filter(|child| is_w_tag(child, "p"));
    let first = inner.next()?;

    // The first paragraph's properties.
    let own_properties = element_children(first).find(|child| is_w_tag(child, "pPr"));
    let mut properties = match (own_properties, outer_properties) {
        (Some(own), _) => own.clone(),
        (None, Some(outer)) => {
            let mut outer = outer.clone();
            outer
                .children
                .retain(|node| !matches!(node, XMLNode::Element(el) if is_w_tag(el, "rPr")));
            outer
        }
        (None, None) => w_el("pPr"),
    };
    if let Some(run_properties) = run_properties {
        let own_mark = element_children(&properties)
            .find(|child| is_w_tag(child, "rPr"))
            .cloned();
        let mut mark = run_properties.clone();
        if let Some(own_mark) = own_mark {
            for property in element_children(&own_mark) {
                mark.children.retain(
                    |node| !matches!(node, XMLNode::Element(el) if el.name == property.name),
                );
                mark.children.push(XMLNode::Element(property.clone()));
            }
        }
        properties
            .children
            .retain(|node| !matches!(node, XMLNode::Element(el) if is_w_tag(el, "rPr")));
        properties.children.push(XMLNode::Element(mark));
    }

    let mut first_paragraph = first.clone();
    first_paragraph.children = Vec::new();
    if !properties.children.is_empty() || !properties.attributes.is_empty() {
        first_paragraph.children.push(XMLNode::Element(properties));
    }
    for node in &paragraph.children[..index] {
        if !matches!(node, XMLNode::Element(el) if is_w_tag(el, "pPr")) {
            first_paragraph.children.push(node.clone());
        }
    }
    if !leading_text.is_empty() {
        let mut lead_text = text.clone();
        lead_text.children = vec![XMLNode::Text(leading_text)];
        let mut lead_run = run.clone();
        lead_run.children = run_properties
            .iter()
            .map(|rpr| XMLNode::Element((*rpr).clone()))
            .collect();
        lead_run.children.push(XMLNode::Element(lead_text));
        first_paragraph.children.push(XMLNode::Element(lead_run));
    }
    first_paragraph.children.extend(
        first
            .children
            .iter()
            .filter(|node| !matches!(node, XMLNode::Element(el) if is_w_tag(el, "pPr")))
            .cloned(),
    );

    let mut out = vec![first_paragraph];
    out.extend(inner.cloned());
    let mut remainder = paragraph.clone();
    remainder.children = outer_properties
        .iter()
        .map(|ppr| XMLNode::Element((*ppr).clone()))
        .collect();
    remainder
        .children
        .extend(paragraph.children[index + 1..].iter().cloned());
    out.push(remainder);
    Some(out)
}

pub(crate) fn paragraphs_in_text_diagnostic(context: String) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Warning,
        message: "tolerated schema-invalid <w:p> inside <w:t>: CT_Text holds text only, but \
                  Word opens such documents without repair and shows the inner paragraphs — \
                  read as Word reads them: the inner paragraphs, then the outer paragraph \
                  emptied of the run"
            .to_string(),
        context: Some(context),
    }
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
