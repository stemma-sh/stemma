//! Opaque-interior discovery — the read-side enumeration of editable text inside
//! opaque regions (a textbox's `w:txbxContent`, an inline content control's
//! `w:sdtContent`), plus the single traversal that both this discovery and the
//! audit revision census share so they can never disagree on WHICH opaques carry
//! an interior.
//!
//! RFC-0002 decision #1: discovery MUST share the census's opaque-interior walk
//! rather than add a parallel traversal — two walks would drift (the audit
//! lesson that produced the census/resolvable split). [`visit_opaque_interiors`]
//! is that one walk; `tracked_model::enumerate_revisions` (the census) and
//! [`opaque_text_targets`] (discovery) are its two consumers. The walk decides
//! which opaque nodes exist and where; each consumer filters for its own purpose
//! (the census: any inline with tracked markup + quarantined blocks; discovery:
//! textbox/SDT text). The reach — body, every story, and recursively through
//! table cells — is defined here, once.
//!
//! Addressing is by the opaque node's own [`NodeId`] plus an [`InteriorAddress`]
//! (container + paragraph index) the edit verb re-navigates against a fresh parse
//! of the fragment. Block-guard hashing catches structural drift between a
//! discovered address and a later edit.

use xmltree::{Element, XMLNode};

use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueBlockNode, OpaqueInlineNode, OpaqueKind,
    StoryScope, TrackedBlock,
};

// ─── The single shared opaque-interior traversal ────────────────────────────

/// One opaque interior yielded by [`visit_opaque_interiors`]: either an inline
/// widget (drawing/pict/object/inline-SDT — carries its bytes in `raw_xml`) or a
/// body-level block opaque (whose bytes live in the serialize scaffold, not on
/// the node, so there is no `raw_xml` to read at the `CanonDoc` level).
pub(crate) enum OpaqueInteriorRef<'a> {
    Inline(&'a OpaqueInlineNode),
    Block(&'a OpaqueBlockNode),
}

/// Visit every opaque node in the document — body, every header/footer/foot-
/// note/endnote/comment story, and recursively through table cells — in document
/// order. This is the canonical reach both the revision census and text-target
/// discovery consume (RFC-0002 §decision-1). `host_block_id` is the enclosing
/// paragraph's id for an inline opaque, or the block's own id for a block opaque.
pub(crate) fn visit_opaque_interiors<F>(doc: &CanonDoc, f: &mut F)
where
    F: FnMut(&NodeId, &StoryScope, OpaqueInteriorRef<'_>),
{
    visit_blocks(&doc.blocks, &StoryScope::Body, f);
    for s in &doc.headers {
        visit_blocks(
            &s.blocks,
            &StoryScope::Header {
                part_path: s.part_name.clone(),
                kind: s.kind.clone(),
            },
            f,
        );
    }
    for s in &doc.footers {
        visit_blocks(
            &s.blocks,
            &StoryScope::Footer {
                part_path: s.part_name.clone(),
                kind: s.kind.clone(),
            },
            f,
        );
    }
    for s in &doc.footnotes {
        visit_blocks(&s.blocks, &StoryScope::Footnote { id: s.id.clone() }, f);
    }
    for s in &doc.endnotes {
        visit_blocks(&s.blocks, &StoryScope::Endnote { id: s.id.clone() }, f);
    }
    for s in &doc.comments {
        visit_blocks(&s.blocks, &StoryScope::Comment { id: s.id.clone() }, f);
    }
}

fn visit_blocks<F>(blocks: &[TrackedBlock], location: &StoryScope, f: &mut F)
where
    F: FnMut(&NodeId, &StoryScope, OpaqueInteriorRef<'_>),
{
    for tb in blocks {
        visit_block(&tb.block, location, f);
    }
}

fn visit_block<F>(block: &BlockNode, location: &StoryScope, f: &mut F)
where
    F: FnMut(&NodeId, &StoryScope, OpaqueInteriorRef<'_>),
{
    match block {
        BlockNode::Paragraph(p) => {
            for inline in p.all_inlines() {
                if let InlineNode::OpaqueInline(o) = inline {
                    f(&p.id, location, OpaqueInteriorRef::Inline(o));
                }
            }
        }
        BlockNode::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    for nested in &cell.blocks {
                        visit_block(nested, location, f);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(o) => {
            f(&o.id, location, OpaqueInteriorRef::Block(o));
        }
    }
}

// ─── Text-target discovery ──────────────────────────────────────────────────

/// What kind of editable text region a target names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpaqueTextTargetKind {
    /// A text-bearing `w:p` inside a textbox's `w:txbxContent`.
    TextboxParagraph,
    /// The text region of an inline content control's `w:sdtContent`.
    InlineSdtText,
}

/// Stable address of an editable text region inside an opaque fragment. The edit
/// verb re-parses the opaque's `raw_xml` and navigates by these indices, so the
/// address stays valid as long as the fragment's text structure does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteriorAddress {
    /// Which container within the fragment: the Nth DISTINCT (text-deduped)
    /// `w:txbxContent`, or the Nth `w:sdtContent`, in document order. Textbox
    /// Choice/Fallback copies share one container index (they are one logical
    /// interior the edit mirrors across).
    pub container_index: usize,
    /// For a textbox, the 0-based index of the text-bearing `w:p` within that
    /// container. For an inline SDT text region, always 0 (the region is one
    /// addressable unit).
    pub paragraph_index: usize,
}

/// A discovered editable text region inside an opaque node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpaqueTextTarget {
    /// The story the hosting opaque lives in.
    pub location: StoryScope,
    /// The enclosing paragraph's id (the block that hosts the opaque inline).
    pub host_block_id: NodeId,
    /// The opaque node's own id — the handle the edit verb addresses.
    pub opaque_id: NodeId,
    pub kind: OpaqueTextTargetKind,
    pub address: InteriorAddress,
    /// The region's current visible `w:t` text. NOTE: includes text inside
    /// PENDING tracked changes (a `w:ins` reads as its inserted text) — the
    /// as-shown view. When `has_tracked_changes` is set, an edit of this
    /// region refuses (`RegionHasTrackedChanges`) until those are resolved.
    pub text: String,
    /// The region already carries tracked-change markup in its editable text
    /// (`opaque_splice::region_has_tracked_containers` — the same predicate
    /// the edit verb refuses on). Surfaced so a caller knows the text is
    /// readable but not editable until the pending changes are resolved,
    /// instead of discovering it via the refusal.
    pub has_tracked_changes: bool,
}

/// Enumerate every editable interior text region reachable at the `CanonDoc`
/// level: textbox paragraphs and inline-SDT text regions. Body-level (block) SDTs
/// carry no `raw_xml` here — their discovery/fill is a runtime concern that needs
/// the serialize scaffold and is handled at that layer.
pub fn opaque_text_targets(doc: &CanonDoc) -> Vec<OpaqueTextTarget> {
    let mut out = Vec::new();
    visit_opaque_interiors(doc, &mut |host, location, interior| {
        if let OpaqueInteriorRef::Inline(o) = interior {
            collect_inline_targets(o, host, location, &mut out);
        }
    });
    out
}

/// How `sdt_text_fill` addresses a block-level content control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockSdtAddress {
    /// An open control, whose content is ordinary blocks: the id of its first
    /// block (the fill's `block_id`, with no `sdt_id`).
    FirstBlock(NodeId),
    /// A sealed control, preserved as one opaque body block: its body index
    /// (the fill's `body_index`).
    BodyIndex(usize),
}

/// A fillable body-level (block) content control. RFC-0002 §Phase-2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSdtTextTarget {
    /// The `sdt_text_fill` address.
    pub address: BlockSdtAddress,
    /// The control's `w:sdtPr/w:tag` value, if any.
    pub tag: Option<String>,
    /// The control's `w:sdtPr/w:alias` value, if any.
    pub alias: Option<String>,
    /// The control's current visible text.
    pub text: String,
}

/// A legacy form field inside an opaque body block. Names are descriptive,
/// not identities: Word permits repeated and empty names. The paragraph and
/// field ordinals are valid only with this block's semantic hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockFormFieldTarget {
    pub body_index: usize,
    pub paragraph_index: usize,
    pub field_index: usize,
    pub name: Option<String>,
    pub instruction: String,
    /// Visible cached result text, not textInput/default or checkbox state.
    pub cached_result: String,
    pub paragraph_text: String,
    pub semantic_hash: String,
    pub(crate) begin_path: Vec<usize>,
    pub(crate) separate_path: Option<Vec<usize>>,
    pub(crate) end_path: Vec<usize>,
    pub(crate) result_text_paths: Vec<Vec<usize>>,
}

/// Read form-field boundaries without flattening the containing control into
/// the editable body model. Malformed boundaries are errors, not empty forms.
pub(crate) fn block_form_field_targets(
    body_index: usize,
    node: &XMLNode,
) -> Result<Vec<BlockFormFieldTarget>, String> {
    let XMLNode::Element(root) = node else {
        return Ok(Vec::new());
    };
    let raw = crate::word_xml::serialize_raw_fragment(root);
    let semantic_hash = crate::import::sha256_hex(&raw);
    fn paragraphs<'a>(
        e: &'a Element,
        path: &mut Vec<usize>,
        out: &mut Vec<(&'a Element, Vec<usize>)>,
    ) {
        if is_w(e, "p") {
            out.push((e, path.clone()));
            return;
        }
        for (index, child) in e.children.iter().enumerate() {
            if let XMLNode::Element(child) = child {
                path.push(index);
                paragraphs(child, path, out);
                path.pop();
            }
        }
    }
    struct Field {
        ordinal: usize,
        name: Option<String>,
        is_form: bool,
        instruction: String,
        value: String,
        separated: bool,
        begin_path: Vec<usize>,
        separate_path: Option<Vec<usize>>,
        end_path: Vec<usize>,
        result_text_paths: Vec<Vec<usize>>,
    }
    fn walk(
        e: &Element,
        stack: &mut Vec<Field>,
        next: &mut usize,
        done: &mut Vec<Field>,
        path: &mut Vec<usize>,
    ) -> Result<(), String> {
        if is_w(e, "del") {
            return Ok(());
        }
        if is_w(e, "fldChar") {
            match crate::xml_attrs::attr_get(e, "w:fldCharType").map(String::as_str) {
                Some("begin") => {
                    let data = child_by_local(e, "ffData");
                    let name = data
                        .and_then(|d| child_by_local(d, "name"))
                        .and_then(|n| crate::xml_attrs::attr_get(n, "w:val").cloned());
                    stack.push(Field {
                        ordinal: *next,
                        name,
                        is_form: data.is_some(),
                        instruction: String::new(),
                        value: String::new(),
                        separated: false,
                        begin_path: path.clone(),
                        separate_path: None,
                        end_path: Vec::new(),
                        result_text_paths: Vec::new(),
                    });
                    *next += 1;
                }
                Some("separate") => {
                    let current = stack.last_mut().ok_or("field separator without begin")?;
                    if current.separated {
                        return Err("duplicate field separator".into());
                    }
                    current.separated = true;
                    current.separate_path = Some(path.clone());
                }
                Some("end") => {
                    let mut field = stack.pop().ok_or("field end without begin")?;
                    field.end_path = path.clone();
                    done.push(field);
                }
                _ => return Err("missing or unknown field character type".into()),
            }
            return Ok(());
        }
        if is_w(e, "instrText") {
            if let Some(current) = stack.last_mut()
                && !current.separated
            {
                current
                    .instruction
                    .push_str(&e.get_text().unwrap_or_default());
            }
            return Ok(());
        }
        if is_w(e, "t") {
            for current in stack.iter_mut().filter(|f| f.separated) {
                current.value.push_str(&e.get_text().unwrap_or_default());
                current.result_text_paths.push(path.clone());
            }
            return Ok(());
        }
        if is_w(e, "tab") || is_w(e, "br") || is_w(e, "cr") {
            let ch = if is_w(e, "tab") { '\t' } else { '\n' };
            for current in stack.iter_mut().filter(|f| f.separated) {
                current.value.push(ch);
                current.result_text_paths.push(path.clone());
            }
            return Ok(());
        }
        for (index, child) in e.children.iter().enumerate() {
            if let XMLNode::Element(child) = child {
                path.push(index);
                walk(child, stack, next, done, path)?;
                path.pop();
            }
        }
        Ok(())
    }
    fn text(e: &Element, out: &mut String) {
        if is_w(e, "del") {
            return;
        }
        if is_w(e, "t") {
            out.push_str(&e.get_text().unwrap_or_default());
            return;
        }
        if is_w(e, "tab") || is_w(e, "br") || is_w(e, "cr") {
            out.push(if is_w(e, "tab") { '\t' } else { '\n' });
            return;
        }
        for child in &e.children {
            if let XMLNode::Element(child) = child {
                text(child, out);
            }
        }
    }
    let mut ps = Vec::new();
    paragraphs(root, &mut Vec::new(), &mut ps);
    let mut out = Vec::new();
    for (paragraph_index, (p, mut path)) in ps.into_iter().enumerate() {
        let mut done = Vec::new();
        let mut stack = Vec::new();
        walk(p, &mut stack, &mut 0, &mut done, &mut path).map_err(|e| {
            format!("body_index={body_index}, paragraph_index={paragraph_index}: {e}")
        })?;
        if !stack.is_empty() {
            return Err(format!(
                "body_index={body_index}, paragraph_index={paragraph_index}: field crosses paragraph boundary or lacks end"
            ));
        }
        let mut paragraph_text = String::new();
        text(p, &mut paragraph_text);
        for field in done.into_iter().filter(|f| f.is_form) {
            out.push(BlockFormFieldTarget {
                body_index,
                paragraph_index,
                field_index: field.ordinal,
                name: field.name,
                instruction: field.instruction.trim().into(),
                cached_result: field.value,
                paragraph_text: paragraph_text.clone(),
                semantic_hash: semantic_hash.clone(),
                begin_path: field.begin_path,
                separate_path: field.separate_path,
                end_path: field.end_path,
                result_text_paths: field.result_text_paths,
            });
        }
    }
    out.sort_by_key(|f| (f.paragraph_index, f.field_index));
    Ok(out)
}

/// Replace one FORMTEXT cached result using locations produced by the same
/// walk as discovery. The default, instruction, enclosing control and every
/// element outside the result stay intact. This is a field-value update, not
/// acceptance of any existing revision.
pub(crate) fn set_block_form_field_text(
    node: &mut XMLNode,
    target: &BlockFormFieldTarget,
    value: &str,
) -> Result<(), String> {
    fn at<'a>(e: &'a Element, path: &[usize]) -> Result<&'a Element, String> {
        let mut current = e;
        for index in path {
            current = current
                .children
                .get(*index)
                .and_then(XMLNode::as_element)
                .ok_or("field location no longer identifies an element")?;
        }
        Ok(current)
    }
    fn at_mut<'a>(e: &'a mut Element, path: &[usize]) -> Result<&'a mut Element, String> {
        let mut current = e;
        for index in path {
            current = current
                .children
                .get_mut(*index)
                .and_then(XMLNode::as_mut_element)
                .ok_or("field location no longer identifies an element")?;
        }
        Ok(current)
    }
    fn revision_tag(e: &Element) -> bool {
        ["ins", "del", "moveFrom", "moveTo", "rPrChange"]
            .iter()
            .any(|tag| is_w(e, tag))
    }
    fn revisions(e: &Element) -> bool {
        revision_tag(e)
            || e.children
                .iter()
                .filter_map(XMLNode::as_element)
                .any(revisions)
    }
    let XMLNode::Element(root) = node else {
        return Err("field container is not an element".into());
    };
    if crate::import::sha256_hex(&crate::word_xml::serialize_raw_fragment(root))
        != target.semantic_hash
    {
        return Err("stale containing-block hash; inspect fields again".into());
    }
    let begin = at(root, &target.begin_path)?;
    let data = child_by_local(begin, "ffData").ok_or("field begin lacks ffData")?;
    if target.instruction != "FORMTEXT"
        || child_by_local(data, "textInput").is_none()
        || child_by_local(data, "checkBox").is_some()
        || child_by_local(data, "ddList").is_some()
    {
        return Err("text update requires an unambiguous FORMTEXT/textInput field".into());
    }
    if target.cached_result == value {
        return Err("field text update has no effect".into());
    }
    let separate = target
        .separate_path
        .as_ref()
        .ok_or("text field lacks result separator")?;
    let end = &target.end_path;
    if separate.len() < 2 || end.len() < 2 || separate[..separate.len() - 2] != end[..end.len() - 2]
    {
        return Err(
            "field result crosses run containers; cannot replace it without changing wrappers"
                .into(),
        );
    }
    let parent_path = &separate[..separate.len() - 2];
    for length in 0..=parent_path.len() {
        if revision_tag(at(root, &parent_path[..length])?) {
            return Err(
                "field result is inside an unresolved revision; no implicit rewrite allowed".into(),
            );
        }
    }
    let first = separate[separate.len() - 2];
    let last = end[end.len() - 2];
    if first > last {
        return Err("field end precedes result separator".into());
    }
    let parent = at(root, parent_path)?;
    let mut prefix = at(root, &separate[..separate.len() - 1])?.clone();
    let mut suffix = at(root, &end[..end.len() - 1])?.clone();
    if !is_w(&prefix, "r") || !is_w(&suffix, "r") {
        return Err("field anchors must be run children".into());
    }
    if parent.children[first..=last]
        .iter()
        .filter_map(XMLNode::as_element)
        .any(revisions)
    {
        return Err(
            "field result carries unresolved revisions; no implicit resolution allowed".into(),
        );
    }
    // The value owns only cached text, tabs, and breaks. Bookmarks, comment
    // boundaries, nested fields, references, and other objects are not part of
    // that value and must never disappear in the replacement splice.
    for (index, child) in parent
        .children
        .iter()
        .enumerate()
        .take(last + 1)
        .skip(first)
    {
        if matches!(child, XMLNode::Text(text) if text.trim().is_empty()) {
            continue;
        }
        let run = child
            .as_element()
            .filter(|e| is_w(e, "r"))
            .ok_or("field result contains a non-run anchor or wrapper; cannot replace as text")?;
        let start = if index == first {
            separate[separate.len() - 1] + 1
        } else {
            0
        };
        let stop = if index == last {
            end[end.len() - 1]
        } else {
            run.children.len()
        };
        for item in run.children.iter().take(stop).skip(start) {
            let allowed = match item {
                XMLNode::Element(e) => ["rPr", "t", "tab", "br", "cr"]
                    .iter()
                    .any(|tag| is_w(e, tag)),
                XMLNode::Text(text) => text.trim().is_empty(),
                _ => false,
            };
            if !allowed {
                return Err(
                    "field result contains a non-text object or anchor; cannot replace as text"
                        .into(),
                );
            }
        }
    }
    // For an empty result, the separator run owns the insertion-point format.
    // Otherwise use the first cached text run, never paragraph defaults.
    let mut replacement = if let Some(path) = target.result_text_paths.first() {
        at(root, &path[..path.len() - 1])?.clone()
    } else {
        prefix.clone()
    };
    if !is_w(&replacement, "r") {
        return Err("cached text is not in a direct run".into());
    }
    replacement
        .children
        .retain(|n| n.as_element().is_some_and(|e| is_w(e, "rPr")));
    fn append_text(run: &mut Element, value: &str) {
        let mut text = crate::word_xml::w_el("t");
        crate::xml_attrs::attr_set(&mut text, "xml:space", "preserve");
        text.children.push(XMLNode::Text(value.into()));
        run.children.push(XMLNode::Element(text));
    }
    // A literal newline/tab in w:t is not a Word line break/tab. Author the
    // native run children so the requested value survives readback exactly.
    if value.contains('\r') {
        return Err("use LF, not CR, for field line breaks".into());
    }
    let mut start = 0;
    for (index, ch) in value.char_indices() {
        if ch == '\n' || ch == '\t' {
            append_text(&mut replacement, &value[start..index]);
            replacement
                .children
                .push(XMLNode::Element(crate::word_xml::w_el(if ch == '\t' {
                    "tab"
                } else {
                    "br"
                })));
            start = index + ch.len_utf8();
        }
    }
    append_text(&mut replacement, &value[start..]);
    prefix.children.truncate(separate[separate.len() - 1] + 1);
    let end_child = end[end.len() - 1];
    suffix.children = suffix
        .children
        .into_iter()
        .enumerate()
        .filter(|(i, n)| *i >= end_child || n.as_element().is_some_and(|e| is_w(e, "rPr")))
        .map(|(_, n)| n)
        .collect();
    // All checks and replacement construction precede the single mutation.
    at_mut(root, parent_path)?.children.splice(
        first..=last,
        [
            XMLNode::Element(prefix),
            XMLNode::Element(replacement),
            XMLNode::Element(suffix),
        ],
    );
    Ok(())
}

/// Project a body-level opaque child node into a block content-control target, if
/// it is a `w:sdt` carrying text. `None` for non-SDT opaque children (body-level
/// bookmark markers, quarantined blocks) or an empty control.
pub(crate) fn block_sdt_target(body_index: usize, node: &XMLNode) -> Option<BlockSdtTextTarget> {
    let XMLNode::Element(el) = node else {
        return None;
    };
    if !is_w(el, "sdt") {
        return None;
    }
    let content = child_by_local(el, "sdtContent")?;
    // The fill sets the FIRST text-bearing paragraph; discover exactly that, and
    // only if it is cleanly fillable (all text in direct simple runs). A control
    // whose value hides in a hyperlink/field is not advertised (would relocate
    // that text on set) — matching the inline path and the `set_region_text` guard.
    let para = first_text_paragraph(content)?;
    let text = crate::opaque_splice::fillable_text(para)?;
    if text.is_empty() {
        return None;
    }
    let sdt_pr = child_by_local(el, "sdtPr");
    let val_of = |local: &str| -> Option<String> {
        sdt_pr
            .and_then(|pr| child_by_local(pr, local))
            .and_then(|e| crate::xml_attrs::attr_get(e, "w:val").cloned())
    };
    Some(BlockSdtTextTarget {
        address: BlockSdtAddress::BodyIndex(body_index),
        tag: val_of("tag"),
        alias: val_of("alias"),
        text,
    })
}

/// The fillable OPEN block content controls of the body, in document order:
/// each control that starts on a top-level block and whose fill
/// (`sdt_text_fill` with that `block_id`) would be accepted, with the value it
/// would replace. A control nested inside another is not separately
/// addressable.
pub(crate) fn open_block_sdt_targets(doc: &CanonDoc) -> Vec<BlockSdtTextTarget> {
    doc.blocks
        .iter()
        .filter_map(|tracked| {
            let wrap = tracked.block_sdt_wrap.as_ref()?;
            let block_id = crate::edit::block_id_of(&tracked.block).clone();
            // The fill's own rule decides what is fillable; a refusal means
            // there is no value to advertise.
            let crate::edit::EditStep::ReplaceSpanText { expect, .. } =
                crate::edit::verbs::sdt_text_fill::lower_open_block_fill(
                    doc, &block_id, "-", None, &None, 0,
                )
                .ok()?
            else {
                unreachable!("an open block fill lowers to a span replace")
            };
            let sdt_pr = crate::word_xml::parse_raw_fragment(&wrap.wrapper.sdt_pr_xml)
                .expect("a wrapper's sdtPr bytes were serialized from a parsed element");
            let val_of = |local: &str| -> Option<String> {
                child_by_local(&sdt_pr, local)
                    .and_then(|e| crate::xml_attrs::attr_get(e, "w:val").cloned())
            };
            Some(BlockSdtTextTarget {
                address: BlockSdtAddress::FirstBlock(block_id),
                tag: val_of("tag"),
                alias: val_of("alias"),
                text: expect.expect("an open block fill pins the current value"),
            })
        })
        .collect()
}

fn child_by_local<'a>(parent: &'a Element, local: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|c| match c {
        XMLNode::Element(e) if is_w(e, local) => Some(e),
        _ => None,
    })
}

/// The first `w:p` in `root`'s subtree (self included) carrying any `w:t` text —
/// the immutable twin of `opaque_splice::first_text_paragraph_mut`, so block-SDT
/// discovery inspects exactly the paragraph the fill will set.
fn first_text_paragraph(root: &Element) -> Option<&Element> {
    if is_w(root, "p") {
        let mut t = String::new();
        collect_wt_text(root, &mut t);
        if !t.is_empty() {
            return Some(root);
        }
    }
    root.children.iter().find_map(|c| match c {
        XMLNode::Element(e) => first_text_paragraph(e),
        _ => None,
    })
}

fn collect_inline_targets(
    o: &OpaqueInlineNode,
    host: &NodeId,
    location: &StoryScope,
    out: &mut Vec<OpaqueTextTarget>,
) {
    // Discovery is read-only enumeration: an opaque with no bytes, or bytes we
    // cannot parse, simply exposes no editable region (the edit verb would refuse
    // it anyway). This is not a silent fallback — there is genuinely nothing
    // addressable, and the census reports any hidden revisions separately.
    let Some(raw) = &o.raw_xml else { return };
    let Ok(fragment) = crate::word_xml::OpaqueFragment::parse(raw) else {
        return;
    };
    let root = fragment.active();
    match &o.kind {
        // Textboxes import as `Drawing` whether DrawingML (`wps:txbx`) or VML
        // (`w:pict` → `v:textbox`); both carry a `w:txbxContent` story.
        OpaqueKind::Drawing => collect_textbox_targets(root, o, host, location, out),
        OpaqueKind::Sdt => collect_inline_sdt_target(root, o, host, location, out),
        _ => {}
    }
}

fn collect_textbox_targets(
    root: &Element,
    o: &OpaqueInlineNode,
    host: &NodeId,
    location: &StoryScope,
    out: &mut Vec<OpaqueTextTarget>,
) {
    let mut contents = Vec::new();
    crate::opaque_meta::collect_descendants_by_local(root, "txbxContent", &mut contents);
    // Dedupe Choice/Fallback copies by their VISIBLE-TEXT signature (not bytes):
    // copies whose paragraph texts agree are one logical interior the edit
    // mirrors across, so they share one container index (matching
    // `opaque_meta::textbox_interior_text` and the edit verb's mirror match).
    // Copies with DIVERGENT text get separate container indices — separately
    // addressable, never silently merged.
    let mut seen: Vec<String> = Vec::new();
    for content in contents {
        let paragraphs = textbox_paragraph_texts(content);
        if paragraphs.is_empty() {
            continue;
        }
        let signature = paragraphs.join("\n");
        if seen.contains(&signature) {
            continue;
        }
        let container_index = seen.len();
        seen.push(signature);
        let tracked_flags = textbox_paragraph_tracked_flags(content);
        for (paragraph_index, text) in paragraphs.into_iter().enumerate() {
            out.push(OpaqueTextTarget {
                location: location.clone(),
                host_block_id: host.clone(),
                opaque_id: o.id.clone(),
                kind: OpaqueTextTargetKind::TextboxParagraph,
                address: InteriorAddress {
                    container_index,
                    paragraph_index,
                },
                text,
                has_tracked_changes: tracked_flags.get(paragraph_index).copied().unwrap_or(false),
            });
        }
    }
}

/// Per addressable textbox paragraph (same predicate/order as
/// `textbox_paragraph_texts`): does its editable text already carry
/// tracked-change markup? One flag per surfaced paragraph, index-aligned.
fn textbox_paragraph_tracked_flags(content: &Element) -> Vec<bool> {
    let mut out = Vec::new();
    for child in &content.children {
        if let XMLNode::Element(el) = child
            && is_w(el, "p")
        {
            let mut text = String::new();
            collect_wt_text(el, &mut text);
            if !text.is_empty() {
                out.push(crate::opaque_splice::region_has_tracked_containers(el));
            }
        }
    }
    out
}

fn collect_inline_sdt_target(
    root: &Element,
    o: &OpaqueInlineNode,
    host: &NodeId,
    location: &StoryScope,
    out: &mut Vec<OpaqueTextTarget>,
) {
    let mut contents = Vec::new();
    crate::opaque_meta::collect_descendants_by_local(root, "sdtContent", &mut contents);
    // The outermost `w:sdtContent` is the control's value region; nested SDTs are
    // out of v1 scope (refused loudly at edit time, not surfaced as a target).
    let Some(content) = contents.first() else {
        return;
    };
    // Surface all text-bearing inline controls: `opaque_text_edit` can edit their
    // text (descending through a hyperlink/smart tag), and `sdt_text_fill` refuses
    // loud on the non-fillable ones (the `set_region_text` guard). Discovery is
    // "here is editable text", not "here is a fillable value".
    let mut text = String::new();
    collect_wt_text(content, &mut text);
    if text.is_empty() {
        return;
    }
    out.push(OpaqueTextTarget {
        location: location.clone(),
        host_block_id: host.clone(),
        opaque_id: o.id.clone(),
        kind: OpaqueTextTargetKind::InlineSdtText,
        address: InteriorAddress {
            container_index: 0,
            paragraph_index: 0,
        },
        text,
        has_tracked_changes: crate::opaque_splice::region_has_tracked_containers(content),
    });
}

// ─── Interior navigation shared with the edit verb ──────────────────────────

/// The `w:t` text of every DIRECT text-bearing `w:p` child of a `w:txbxContent`,
/// in order. The predicate "has any `w:t` text" is the one both discovery and the
/// edit verb use to index textbox paragraphs, so an address minted here resolves
/// to the same paragraph at edit time. Nested tables inside a textbox are out of
/// v1 scope — only direct paragraph children are addressable.
pub(crate) fn textbox_paragraph_texts(content: &Element) -> Vec<String> {
    let mut out = Vec::new();
    for child in &content.children {
        if let XMLNode::Element(el) = child
            && is_w(el, "p")
        {
            let mut text = String::new();
            collect_wt_text(el, &mut text);
            if !text.is_empty() {
                out.push(text);
            }
        }
    }
    out
}

fn is_w(e: &Element, local: &str) -> bool {
    crate::word_xml::is_w_tag(e, local)
}

/// Concatenate the `w:t` (visible, non-deleted) text under `element`. Deleted
/// text (`w:delText`) is deliberately excluded — it is already a tracked deletion,
/// not editable current content.
pub(crate) fn collect_wt_text(element: &Element, out: &mut String) {
    if is_w(element, "t") {
        for child in &element.children {
            if let XMLNode::Text(t) | XMLNode::CData(t) = child {
                out.push_str(t);
            }
        }
        return;
    }
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            collect_wt_text(el, out);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nested_form_update_refuses_result_anchors_and_revision_ancestors() {
        // A field-value assignment does not authorize removal of bookmarks or
        // comments, nor rewriting a result inside someone else's insertion.
        let field = |result: &str| {
            format!(
                r#"<w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:textInput/></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r>{result}<w:r><w:fldChar w:fldCharType="end"/></w:r>"#
            )
        };
        let plain = "<w:r><w:t>Old</w:t></w:r>";
        let cases = [
            field(&format!(
                r#"<w:bookmarkStart w:id="3" w:name="Keep"/>{plain}<w:bookmarkEnd w:id="3"/>"#
            )),
            field(&format!(
                r#"<w:commentRangeStart w:id="3"/>{plain}<w:commentRangeEnd w:id="3"/>"#
            )),
            field(r#"<w:r><w:t>Old</w:t><w:footnoteReference w:id="4"/></w:r>"#),
            format!(
                r#"<w:ins w:id="2" w:author="Earlier">{}</w:ins>"#,
                field(plain)
            ),
        ];
        for body in cases {
            let xml = format!(
                r#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdtContent><w:p>{body}</w:p></w:sdtContent></w:sdt>"#
            );
            let mut node = xmltree::XMLNode::Element(
                crate::word_xml::parse_raw_fragment(xml.as_bytes()).unwrap(),
            );
            let target = super::block_form_field_targets(0, &node).unwrap().remove(0);
            let before = node.clone();
            assert!(
                super::set_block_form_field_text(&mut node, &target, "New").is_err(),
                "assignment must not erase anchors or rewrite an earlier revision: {body}"
            );
            assert_eq!(node, before, "refusal must leave the container untouched");
        }
    }

    #[test]
    fn nested_form_text_update_preserves_default_formatting_and_other_fields() {
        let field = |name: &str, value: &str| {
            format!(
                r#"<w:p><w:r><w:t>Label:</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="{name}"/><w:textInput><w:default w:val="Original default"/></w:textInput></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:rPr><w:i/><w:sz w:val="22"/></w:rPr><w:t>{value}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>Suffix</w:t></w:r></w:p>"#
            )
        };
        let xml = format!(
            r#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdtContent>{}{}</w:sdtContent></w:sdt>"#,
            field("Duplicate", "Keep"),
            field("Duplicate", "Old")
        );
        let mut node =
            xmltree::XMLNode::Element(crate::word_xml::parse_raw_fragment(xml.as_bytes()).unwrap());
        let targets = super::block_form_field_targets(0, &node).unwrap();
        let original = node.clone();
        super::set_block_form_field_text(&mut node, &targets[1], "Replacement").unwrap();
        let after = super::block_form_field_targets(0, &node).unwrap();
        assert_eq!(after[0].cached_result, "Keep");
        assert_eq!(after[1].cached_result, "Replacement");
        assert_eq!(after[1].paragraph_text, "Label:ReplacementSuffix");
        let xmltree::XMLNode::Element(root) = &node else {
            panic!()
        };
        let raw = String::from_utf8(crate::word_xml::serialize_raw_fragment(root)).unwrap();
        assert_eq!(raw.matches("Original default").count(), 2);
        assert_eq!(raw.matches("w:val=\"22\"").count(), 2);
        let updated = node.clone();
        assert!(super::set_block_form_field_text(&mut node, &targets[1], "Stale update").is_err());
        assert_eq!(node, updated, "failed precondition must not mutate");
        assert_ne!(node, original);
        super::set_block_form_field_text(&mut node, &after[1], "First line\nSecond\tcell").unwrap();
        let multiline = super::block_form_field_targets(0, &node).unwrap();
        assert_eq!(multiline[1].cached_result, "First line\nSecond\tcell");
        let old_run = r#"<w:r><w:rPr><w:i/><w:sz w:val="22"/></w:rPr><w:t>Old</w:t></w:r>"#;
        assert!(xml.contains(old_run));
        let tracked_xml = xml.replace(
            old_run,
            &format!(r#"<w:ins w:id="1" w:author="Earlier">{old_run}</w:ins>"#),
        );
        let mut tracked = xmltree::XMLNode::Element(
            crate::word_xml::parse_raw_fragment(tracked_xml.as_bytes()).unwrap(),
        );
        let targets = super::block_form_field_targets(0, &tracked).unwrap();
        let unchanged = tracked.clone();
        assert!(
            super::set_block_form_field_text(&mut tracked, &targets[1], "Replacement").is_err()
        );
        assert_eq!(
            tracked, unchanged,
            "refusal must not resolve prior revision"
        );
    }

    #[test]
    fn nested_form_discovery_uses_locations_not_repeated_names_or_first_text() {
        let field = |value: &str| {
            format!(
                r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Repeated"/><w:textInput><w:default w:val="Not the live value"/></w:textInput></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>{value}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#
            )
        };
        let xml = format!(
            r#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdtContent><w:p><w:r><w:t>Date</w:t></w:r></w:p>{}{}</w:sdtContent></w:sdt>"#,
            field("First value"),
            field("Second value")
        );
        let root = crate::word_xml::parse_raw_fragment(xml.as_bytes()).unwrap();
        let fields = super::block_form_field_targets(4, &xmltree::XMLNode::Element(root)).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].body_index, 4);
        assert_eq!(fields[0].paragraph_index, 1);
        assert_eq!(fields[1].paragraph_index, 2);
        assert_eq!(fields[0].field_index, 0);
        assert_eq!(fields[0].name, fields[1].name);
        assert_eq!(fields[0].cached_result, "First value");
        assert_eq!(fields[1].cached_result, "Second value");
        assert_eq!(fields[0].instruction, "FORMTEXT");
        assert_eq!(fields[0].semantic_hash, fields[1].semantic_hash);
        assert!(!fields[0].semantic_hash.is_empty());
    }

    #[test]
    fn nested_form_discovery_rejects_unpaired_field_boundaries() {
        let xml = r#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdtContent><w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:textInput/></w:ffData></w:fldChar></w:r></w:p></w:sdtContent></w:sdt>"#;
        let root = crate::word_xml::parse_raw_fragment(xml.as_bytes()).unwrap();
        let error =
            super::block_form_field_targets(2, &xmltree::XMLNode::Element(root)).unwrap_err();
        assert!(error.contains("body_index=2, paragraph_index=0"));
        assert!(error.contains("lacks end"));
    }

    use super::*;
    use crate::domain::CanonDoc;
    use crate::runtime::{DocxRuntime, SimpleRuntime};

    /// Wrap a `w:body` inner fragment in a minimal, valid OPC package and import
    /// it to a `CanonDoc`. Corpus-free — built in-process so the daily gate needs
    /// no fixtures.
    fn import_body(body_inner: &str) -> std::sync::Arc<CanonDoc> {
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

        use std::io::Write;
        use zip::write::FileOptions;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: FileOptions = FileOptions::default();
            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();
            zip.start_file("word/_rels/document.xml.rels", opts)
                .unwrap();
            zip.write_all(doc_rels.as_bytes()).unwrap();
            zip.start_file("word/document.xml", opts).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        SimpleRuntime::new()
            .import_docx(&buf)
            .expect("import synthetic docx")
            .canonical
    }

    /// A `w:p` hosting an inline DrawingML textbox whose interior is `paras`
    /// (each a plain-text paragraph).
    fn textbox_paragraph(paras: &[&str]) -> String {
        let inner: String = paras
            .iter()
            .map(|t| format!(r#"<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"#))
            .collect();
        format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
        )
    }

    #[test]
    fn discovers_textbox_paragraphs_in_order() {
        let doc = import_body(&textbox_paragraph(&["First line", "Second line"]));
        let targets = opaque_text_targets(&doc);
        assert_eq!(targets.len(), 2, "one target per text-bearing textbox ¶");
        assert!(
            targets
                .iter()
                .all(|t| t.kind == OpaqueTextTargetKind::TextboxParagraph)
        );
        assert_eq!(targets[0].text, "First line");
        assert_eq!(targets[0].address.paragraph_index, 0);
        assert_eq!(targets[1].text, "Second line");
        assert_eq!(targets[1].address.paragraph_index, 1);
        // Both paragraphs live in the same textbox → same opaque id & container.
        assert_eq!(targets[0].opaque_id, targets[1].opaque_id);
        assert_eq!(targets[0].address.container_index, 0);
        assert_eq!(targets[1].address.container_index, 0);
    }

    #[test]
    fn discovers_inline_sdt_text() {
        let body = r#"<w:p><w:sdt><w:sdtPr><w:alias w:val="Tenant"/><w:tag w:val="tenant"/></w:sdtPr><w:sdtContent><w:r><w:t>Acme Corp</w:t></w:r></w:sdtContent></w:sdt></w:p>"#;
        let doc = import_body(body);
        let targets = opaque_text_targets(&doc);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, OpaqueTextTargetKind::InlineSdtText);
        assert_eq!(targets[0].text, "Acme Corp");
    }

    #[test]
    fn empty_textbox_yields_no_targets() {
        let doc = import_body(&textbox_paragraph(&[]));
        assert!(opaque_text_targets(&doc).is_empty());
    }

    /// The anti-drift guarantee (RFC-0002 §decision-1): a textbox carrying BOTH
    /// plain text and an interior tracked change is seen by the SAME opaque node
    /// in both the census and discovery — they share one walk, so the census
    /// reports the hidden revision under the host block and discovery exposes the
    /// editable text under that same host block.
    #[test]
    fn census_and_discovery_agree_on_the_same_opaque() {
        let inner = r#"<w:p><w:r><w:t>Visible text </w:t></w:r><w:ins w:id="1" w:author="Vanessa" w:date="2024-01-01T00:00:00Z"><w:r><w:t>added</w:t></w:r></w:ins></w:p>"#;
        let body = format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
        );
        let doc = import_body(&body);

        let census = crate::tracked_model::enumerate_revisions(&doc);
        let opaque_records: Vec<_> = census
            .iter()
            .filter(|r| r.kind == crate::tracked_model::RevisionKind::OpaqueInterior)
            .collect();
        assert!(
            !opaque_records.is_empty(),
            "census must surface the textbox's interior tracked insert"
        );

        let targets = opaque_text_targets(&doc);
        assert!(
            !targets.is_empty(),
            "discovery must surface the textbox's editable text"
        );
        // Same host block in both surfaces — the shared walk visited one opaque.
        assert_eq!(
            opaque_records[0].block_id, targets[0].host_block_id,
            "census and discovery must attribute the same opaque to the same host block"
        );
    }
}
