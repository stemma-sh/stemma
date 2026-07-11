use std::collections::{HashMap, HashSet};

use std::io::Cursor;

use quick_xml::Reader;
use quick_xml::events::Event;
use quick_xml::name::QName;
use xmltree::{AttributeName, Element, Namespace, XMLNode};

use crate::xml_attrs::{attr_get, attr_set};
const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
// Maximum element nesting depth accepted from untrusted XML. This bounds the
// recursive-descent parsers (and the recursive tree passes that follow) so a
// crafted deeply-nested fragment cannot overflow the thread stack — a stack
// overflow is an uncatchable process abort, so this must stay well under what a
// default ~2 MiB worker stack can recurse through. Real WordprocessingML nests
// only tens of levels deep (tables-in-cells-in-tables, content controls), so
// 512 is generously above any legitimate document while remaining stack-safe.
// Kept in sync with `normalize::MAX_XML_ELEMENT_DEPTH`.
const MAX_XML_ELEMENT_DEPTH: usize = 512;

// =============================================================================
// Known OOXML namespace prefix -> URI map
//
// This is the finite set of namespace prefixes used across OOXML documents.
// When opaque content (drawings, objects, fields) is re-emitted, it may use
// prefixes that were declared on an ancestor in the original document but are
// absent in the new tree. This map lets us resolve those prefixes and declare
// them on the root element — a "namespace fixup" linker pass.
// =============================================================================

pub(crate) const KNOWN_OOXML_NAMESPACES: &[(&str, &str)] = &[
    // Core WordprocessingML / OOXML
    (
        "w",
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    ),
    (
        "r",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    ),
    (
        "m",
        "http://schemas.openxmlformats.org/officeDocument/2006/math",
    ),
    (
        "mc",
        "http://schemas.openxmlformats.org/markup-compatibility/2006",
    ),
    // DrawingML
    ("a", "http://schemas.openxmlformats.org/drawingml/2006/main"),
    (
        "wp",
        "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
    ),
    (
        "pic",
        "http://schemas.openxmlformats.org/drawingml/2006/picture",
    ),
    // VML
    ("v", "urn:schemas-microsoft-com:vml"),
    ("o", "urn:schemas-microsoft-com:office:office"),
    ("w10", "urn:schemas-microsoft-com:office:word"),
    // Microsoft Word extensions
    (
        "w14",
        "http://schemas.microsoft.com/office/word/2010/wordml",
    ),
    (
        "w15",
        "http://schemas.microsoft.com/office/word/2012/wordml",
    ),
    (
        "w16",
        "http://schemas.microsoft.com/office/word/2018/wordml",
    ),
    (
        "w16cex",
        "http://schemas.microsoft.com/office/word/2018/wordml/cex",
    ),
    (
        "w16cid",
        "http://schemas.microsoft.com/office/word/2016/wordml/cid",
    ),
    (
        "w16du",
        "http://schemas.microsoft.com/office/word/2023/wordml/word16du",
    ),
    (
        "w16se",
        "http://schemas.microsoft.com/office/word/2015/wordml/symex",
    ),
    (
        "w16sdtp",
        "http://schemas.microsoft.com/office/word/2020/wordml/sdtp",
    ),
    // DrawingML extensions
    (
        "a14",
        "http://schemas.microsoft.com/office/drawing/2010/main",
    ),
    (
        "a16",
        "http://schemas.microsoft.com/office/drawing/2014/main",
    ),
    // WordprocessingDrawing extensions
    (
        "wp14",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing",
    ),
    (
        "wpc",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas",
    ),
    (
        "wpg",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup",
    ),
    (
        "wpi",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingInk",
    ),
    (
        "wps",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingShape",
    ),
    (
        "wne",
        "http://schemas.microsoft.com/office/word/2006/wordml",
    ),
    // Chart / DML extensions
    (
        "c",
        "http://schemas.openxmlformats.org/drawingml/2006/chart",
    ),
    (
        "c14",
        "http://schemas.microsoft.com/office/drawing/2007/8/2/chart",
    ),
    (
        "c16",
        "http://schemas.microsoft.com/office/drawing/2014/chart",
    ),
    (
        "c16r2",
        "http://schemas.microsoft.com/office/drawing/2015/06/chart",
    ),
    (
        "cx",
        "http://schemas.microsoft.com/office/drawing/2014/chartex",
    ),
    // Other common namespaces
    (
        "dgm",
        "http://schemas.openxmlformats.org/drawingml/2006/diagram",
    ),
    (
        "asvg",
        "http://schemas.microsoft.com/office/drawing/2016/SVG/main",
    ),
    (
        "am3d",
        "http://schemas.microsoft.com/office/drawing/2017/model3d",
    ),
];

// Namespace URIs that are "core" OOXML — always understood by conforming
// consumers and never need `mc:Ignorable`. Matches the list in
// `docx_validate_namespaces::CORE_OOXML_NAMESPACES`.
pub(crate) const CORE_NAMESPACE_URIS: &[&str] = &[
    "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    "http://schemas.openxmlformats.org/officeDocument/2006/math",
    "http://schemas.openxmlformats.org/markup-compatibility/2006",
    "http://schemas.openxmlformats.org/drawingml/2006/main",
    "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
    "http://schemas.openxmlformats.org/drawingml/2006/picture",
    "urn:schemas-microsoft-com:vml",
    "urn:schemas-microsoft-com:office:office",
    "urn:schemas-microsoft-com:office:word",
    "http://www.w3.org/XML/1998/namespace",
    "http://www.w3.org/2000/xmlns/",
    // Chart and diagram namespaces are core in their respective parts
    "http://schemas.openxmlformats.org/drawingml/2006/chart",
    "http://schemas.openxmlformats.org/drawingml/2006/diagram",
];

#[derive(Debug)]
pub enum WordXmlError {
    XmlParse(xmltree::ParseError),
    XmlDepthExceeded {
        limit: usize,
        depth: usize,
    },
    XmlWrite(xmltree::Error),
    MissingBody,
    MultipleBody(usize),
    MissingDocument,
    /// quick-xml builder failure with context (byte position + reason).
    QuickXml {
        position: u64,
        reason: String,
    },
    /// DOCTYPE / DTD is rejected outright (entity-expansion defense).
    DoctypeRejected,
    /// The stream ended without producing a root element.
    NoRootElement,
}

pub fn parse_document_xml(bytes: &[u8]) -> Result<Element, WordXmlError> {
    ensure_xml_depth_within_limit(bytes, MAX_XML_ELEMENT_DEPTH)?;
    parse_document_xml_quick(bytes)
}

/// True when `bytes` carry no XML content at all: 0 bytes, a lone UTF-8 BOM, or
/// only ASCII whitespace. This is the exact shape Word emits for an EMPTY
/// running-head part (`word/headerN.xml` / `word/footerN.xml`) — a 0-byte part
/// means the reference resolves to an empty header/footer, which Word renders as
/// no running head.
///
/// It is deliberately narrow: bytes that contain any non-whitespace character —
/// including a bare `<?xml?>` declaration with no root element, or a truncated
/// fragment — are NOT empty. Those are malformed and must still fail loud at
/// `parse_document_xml` (`NoRootElement`); tolerating them would silently
/// swallow a truncated part as if it were empty.
pub fn is_empty_or_whitespace_xml(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    bytes.iter().all(|b| b.is_ascii_whitespace())
}

/// Extract the document-root ANCESTOR elements — `<w:document>` and (if present)
/// `<w:body>` — as CHILDLESS `Element`s carrying only their attributes and
/// namespace declarations. These are the ancestors whose `mc:Ignorable` /
/// `mc:ProcessContent` govern the whole body (ISO/IEC 29500-3 §9.2), and the
/// streaming body importer (`for_each_body_child`) never materializes them. Cheap:
/// stops as soon as the body's first child opens (or the body closes / document
/// ends), so it reads only the two outer start tags, not the body.
pub fn document_root_ancestors(bytes: &[u8]) -> Result<Vec<Element>, WordXmlError> {
    ensure_xml_depth_within_limit(bytes, MAX_XML_ELEMENT_DEPTH)?;

    let mut reader = Reader::from_reader(bytes);
    {
        let config = reader.config_mut();
        config.trim_text(false);
        config.expand_empty_elements = false;
        config.check_end_names = true;
    }

    let mut scope: Vec<NsFrame> = Vec::new();
    let mut ancestors: Vec<Element> = Vec::new();
    let mut depth: usize = 0;
    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("{e}"),
            })?;
        match event {
            Event::Start(ref start) => {
                let (element, frame) = element_from_start(start, &reader, &scope)?;
                if depth == 0 {
                    if !is_w_tag(&element, "document") {
                        return Err(WordXmlError::MissingDocument);
                    }
                    ancestors.push(element); // childless: only attrs + namespaces
                    scope.push(frame);
                    depth += 1;
                } else if depth == 1 && is_w_tag(&element, "body") {
                    ancestors.push(element); // childless
                    // We have document + body — every body child's ancestor scope
                    // is now captured. Stop before descending into the body.
                    return Ok(ancestors);
                } else {
                    // A non-body child of document (e.g. w:background) — keep
                    // scanning siblings at depth 1 without recording it here.
                    // (w:background is captured separately by
                    // `parse_document_background_element`, which materializes it
                    // into CanonDoc.document_background; this scan only collects
                    // the MCE ancestor scope for body children.)
                    scope.push(frame);
                    depth += 1;
                }
            }
            Event::Empty(_) => {
                // A self-closing element at this level carries no ancestor scope
                // for body children; ignore.
            }
            Event::End(_) => {
                // Closed the document (or a depth-1 sibling) before reaching a
                // body — return whatever ancestors we have (just the document).
                if depth <= 1 {
                    return Ok(ancestors);
                }
                scope.pop();
                depth -= 1;
            }
            Event::Eof => return Ok(ancestors),
            _ => {}
        }
    }
}

/// Materialize the `<w:background>` element (ISO 29500-1 §17.2.1) if it is
/// present as a direct child of `<w:document>`.
///
/// `w:background` is a sibling of `w:body`, ordered before it in CT_Document,
/// so the streaming body importer (`for_each_body_child`) never sees it. This
/// scans the document's depth-1 children and, on encountering `w:background`,
/// builds its full subtree (attrs + any VML drawing child) and returns it.
/// Stops at `<w:body>` (background always precedes body). Returns `None` when
/// no `w:background` is present.
pub fn parse_document_background_element(bytes: &[u8]) -> Result<Option<Element>, WordXmlError> {
    ensure_xml_depth_within_limit(bytes, MAX_XML_ELEMENT_DEPTH)?;

    let mut reader = Reader::from_reader(bytes);
    {
        let config = reader.config_mut();
        config.trim_text(false);
        config.expand_empty_elements = false;
        config.check_end_names = true;
    }

    let mut scope: Vec<NsFrame> = Vec::new();
    let mut depth: usize = 0;
    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("{e}"),
            })?;
        match event {
            Event::Start(ref start) => {
                let (element, frame) = element_from_start(start, &reader, &scope)?;
                if depth == 0 {
                    if !is_w_tag(&element, "document") {
                        return Err(WordXmlError::MissingDocument);
                    }
                    scope.push(frame);
                    depth += 1;
                } else if depth == 1 && is_w_tag(&element, "body") {
                    // Reached the body; background (if any) would have preceded it.
                    return Ok(None);
                } else if depth == 1 && is_w_tag(&element, "background") {
                    // Build the full background subtree (attrs + drawing child).
                    scope.push(frame);
                    let subtree =
                        build_subtree_from_open(&mut reader, &mut buf, element, &mut scope)?;
                    return Ok(Some(subtree));
                } else {
                    scope.push(frame);
                    depth += 1;
                }
            }
            Event::Empty(ref start) => {
                let (element, _frame) = element_from_start(start, &reader, &scope)?;
                if depth == 1 && is_w_tag(&element, "background") {
                    // Self-closing <w:background .../> — attrs only, no children.
                    return Ok(Some(element));
                }
                if depth == 1 && is_w_tag(&element, "body") {
                    return Ok(None);
                }
            }
            Event::End(_) => {
                if depth <= 1 {
                    // Closed the document before reaching a body.
                    return Ok(None);
                }
                scope.pop();
                depth -= 1;
            }
            Event::DocType(_) => return Err(WordXmlError::DoctypeRejected),
            Event::Eof => return Ok(None),
            _ => {}
        }
        buf.clear();
    }
}

// =============================================================================
// quick-xml -> xmltree::Element builder (Approach A)
//
// Builds the same `xmltree::Element` tree that the xml-rs path produced, but
// without xml-rs's per-start-element clone of the entire in-scope namespace map
// (`NamespaceStack::squash`), which dominated peak heap on import.
//
// Fidelity contract (must match the previous xml-rs path so the ~51 traversal
// functions in `word_ir.rs` and the xmltree writer keep working unchanged):
//   * `Element.name`      = local name (prefix stripped)
//   * `Element.prefix`    = Some(prefix) when the tag was `prefix:local`, else None
//   * `Element.namespace` = resolved URI for that prefix (so `is_w_tag` matches
//                           via `.namespace == WORD_NS`)
//   * `Element.namespaces`= ONLY the xmlns declarations that literally appear on
//                           this element (typically just the root). We do NOT
//                           clone the inherited scope onto every element — that
//                           is the whole point of Approach A.
//   * attributes preserve insertion order (IndexMap via `attribute-order`),
//     with `local_name` / `prefix` / `namespace` matching the xml-rs `OwnedName`.
//   * text / attribute values are EXPLICITLY unescaped (quick-xml hands back the
//     raw, still-escaped bytes); comments are decoded but NOT unescaped.
//   * whitespace-only text is preserved (equivalent to the previous
//     `whitespace_to_characters(true)`), so `xml:space="preserve"` survives.
//   * comments are preserved (equivalent to `ignore_comments(false)`).
// =============================================================================

const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
const XML_URI: &str = "http://www.w3.org/XML/1998/namespace";

/// One frame of in-scope namespace bindings. Holds only the prefixes *declared
/// on the corresponding element* — never the inherited scope. URI resolution
/// walks the stack from the top, which is what gives us inheritance semantics
/// without ever materializing (and cloning) a flattened scope per element.
struct NsFrame {
    /// `prefix -> uri`. The default namespace uses the empty-string prefix.
    bindings: Vec<(String, String)>,
}

/// Resolve a prefix to its URI by scanning declared frames from innermost out.
/// `xml` and `xmlns` are always implicitly bound per the XML Namespaces spec.
fn resolve_uri<'a>(scope: &'a [NsFrame], prefix: &str) -> Option<&'a str> {
    if prefix == "xml" {
        return Some(XML_URI);
    }
    if prefix == "xmlns" {
        return Some(XMLNS);
    }
    for frame in scope.iter().rev() {
        for (p, uri) in &frame.bindings {
            if p == prefix {
                return Some(uri.as_str());
            }
        }
    }
    None
}

/// Split a `QName` into `(prefix, local)` as owned strings via lossless UTF-8.
/// OOXML names are always ASCII, but we decode defensively rather than assume.
fn split_qname(qname: QName<'_>) -> Result<(Option<String>, String), WordXmlError> {
    let (local, prefix) = qname.decompose();
    let local = std::str::from_utf8(local.into_inner())
        .map_err(|e| WordXmlError::QuickXml {
            position: 0,
            reason: format!("non-UTF-8 element/attribute local name: {e}"),
        })?
        .to_string();
    let prefix = match prefix {
        Some(p) => Some(
            std::str::from_utf8(p.into_inner())
                .map_err(|e| WordXmlError::QuickXml {
                    position: 0,
                    reason: format!("non-UTF-8 namespace prefix: {e}"),
                })?
                .to_string(),
        ),
        None => None,
    };
    Ok((prefix, local))
}

/// Build an `Element` from a quick-xml start tag's bytes: split the name,
/// partition attributes into xmlns-declarations (kept as this element's
/// `namespaces` map) and ordinary attributes (kept in insertion order), and
/// resolve namespace URIs against the current scope.
///
/// Returns the new element plus the `NsFrame` of declarations it introduced so
/// the caller can push it onto the scope stack for the element's subtree.
fn element_from_start(
    start: &quick_xml::events::BytesStart<'_>,
    reader: &Reader<&[u8]>,
    scope: &[NsFrame],
) -> Result<(Element, NsFrame), WordXmlError> {
    let decoder = reader.decoder();
    let (prefix, name) = split_qname(start.name())?;

    // First pass over attributes: harvest xmlns declarations so we can resolve
    // this element's own prefix against them (xmlns on an element is in scope
    // for that element itself).
    let mut ns_decls: Vec<(String, String)> = Vec::new();
    let mut plain_attrs: Vec<(AttributeName, String)> = Vec::new();

    for attr in start.attributes() {
        let attr = attr.map_err(|e| WordXmlError::QuickXml {
            position: reader.buffer_position(),
            reason: format!("malformed attribute: {e}"),
        })?;
        let key = attr.key;
        let value = attr
            .unescape_value()
            .map_err(|e| WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("attribute value unescape failed: {e}"),
            })?
            .into_owned();

        if key.as_ref() == b"xmlns" {
            // Default namespace declaration.
            ns_decls.push((String::new(), value));
            continue;
        }
        let (akey_prefix, akey_local) = split_qname(key)?;
        if akey_prefix.as_deref() == Some("xmlns") {
            // `xmlns:foo` -> declares prefix `foo`.
            ns_decls.push((akey_local, value));
            continue;
        }

        // Ordinary attribute. Defer namespace-URI resolution until after we've
        // seen this element's own declarations (an attribute may use a prefix
        // declared on the very same element).
        plain_attrs.push((
            AttributeName {
                local_name: akey_local,
                namespace: None,
                prefix: akey_prefix,
            },
            value,
        ));
    }

    // Frame of declarations introduced by this element. Pushed by the caller.
    let frame = NsFrame { bindings: ns_decls };

    // Resolve the element's own namespace URI: check this element's declarations
    // first, then the inherited scope.
    let elem_prefix_str = prefix.as_deref().unwrap_or("");
    let namespace = resolve_in_frame_then_scope(&frame, scope, elem_prefix_str);

    // Resolve attribute namespace URIs. Unprefixed attributes are NOT in any
    // namespace (per XML Namespaces — the default namespace does not apply to
    // attributes), matching xml-rs's behavior.
    let mut attributes =
        xmltree::AttributeMap::<AttributeName, String>::with_capacity(plain_attrs.len());
    for (mut an, val) in plain_attrs {
        if let Some(ref p) = an.prefix {
            an.namespace = resolve_in_frame_then_scope(&frame, scope, p).map(str::to_string);
        }
        attributes.insert(an, val);
    }

    // Build the `namespaces` map from declarations on this element.
    //
    // For an `mc:Choice`, additionally HOIST the in-scope bindings of every
    // prefix named in its `Requires` attribute. ISO/IEC 29500-3 §7.6/§9.3:
    // `Requires` lists namespace PREFIXES that must be resolved (through the
    // in-scope xmlns bindings) to namespace NAMES before selection. The Approach-A
    // parser keeps only literal-on-element declarations per node (to avoid xml-rs's
    // per-element scope clone), so the binding for a Requires prefix declared on an
    // ANCESTOR would otherwise be invisible to `mc_choice_is_selectable` (which
    // runs later, over the built tree, with no scope stack). Hoisting those exact
    // bindings down onto the Choice makes selection resolvable from the node alone.
    // These entries are dropped on serialization (`serialize_element` rebuilds
    // xmlns decls from USED prefixes), so they never leak into round-tripped bytes.
    let mut hoisted_requires: Vec<(String, String)> = Vec::new();
    if name == "Choice"
        && prefix.as_deref() == Some("mc")
        && let Some((_, requires)) = attributes
            .iter()
            .find(|(an, _)| an.prefix.is_none() && an.local_name == "Requires")
    {
        for token in requires.split_whitespace() {
            let already_local = frame.bindings.iter().any(|(p, _)| p == token);
            if already_local {
                continue;
            }
            if let Some(uri) = resolve_uri(scope, token) {
                hoisted_requires.push((token.to_string(), uri.to_string()));
            }
        }
    }

    let namespaces = if frame.bindings.is_empty() && hoisted_requires.is_empty() {
        None
    } else {
        let mut ns = Namespace::empty();
        for (p, uri) in &frame.bindings {
            ns.put(p.as_str(), uri.as_str());
        }
        for (p, uri) in &hoisted_requires {
            ns.put(p.as_str(), uri.as_str());
        }
        Some(ns)
    };

    let _ = decoder; // decoder reserved for future encodings; names are UTF-8.

    let element = Element {
        prefix,
        namespace: namespace.map(str::to_string),
        namespaces,
        name,
        attributes,
        children: Vec::new(),
    };
    Ok((element, frame))
}

/// Resolve a prefix against this element's own declarations first, then the
/// enclosing scope. Used for both the element name and prefixed attributes.
fn resolve_in_frame_then_scope<'a>(
    frame: &'a NsFrame,
    scope: &'a [NsFrame],
    prefix: &str,
) -> Option<&'a str> {
    if prefix == "xml" {
        return Some(XML_URI);
    }
    if prefix == "xmlns" {
        return Some(XMLNS);
    }
    for (p, uri) in &frame.bindings {
        if p == prefix {
            return Some(uri.as_str());
        }
    }
    resolve_uri(scope, prefix)
}

/// Parse `document.xml` bytes into an `xmltree::Element` using quick-xml.
///
/// This is the Approach-A replacement for the xml-rs `parse_with_config` path.
/// It produces a tree byte-compatible with the previous parser for everything
/// the engine reads and re-serializes, while avoiding the per-element namespace
/// clone that dominated peak heap.
pub fn parse_document_xml_quick(bytes: &[u8]) -> Result<Element, WordXmlError> {
    let mut reader = Reader::from_reader(bytes);
    {
        let config = reader.config_mut();
        // Preserve whitespace-only text (equivalent to whitespace_to_characters):
        // do NOT trim. xml:space="preserve" runs must survive verbatim.
        config.trim_text(false);
        // Keep <a/> as a single Empty event rather than synthesizing End — we
        // handle Empty explicitly, so leave it off.
        config.expand_empty_elements = false;
        // We validate structure ourselves; mismatched ends are a parse error.
        config.check_end_names = true;
    }

    // Stack of partially built elements. `stack.last_mut()` is the open parent.
    let mut stack: Vec<Element> = Vec::new();
    // Parallel stack of namespace-declaration frames (one per open element).
    let mut scope: Vec<NsFrame> = Vec::new();
    let mut root: Option<Element> = None;

    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("{e}"),
            })?;

        match event {
            Event::Start(ref start) => {
                let (element, frame) = element_from_start(start, &reader, &scope)?;
                stack.push(element);
                scope.push(frame);
            }
            Event::Empty(ref start) => {
                let (element, _frame) = element_from_start(start, &reader, &scope)?;
                push_child(&mut stack, &mut root, element)?;
            }
            Event::End(_) => {
                let finished = stack.pop().ok_or_else(|| WordXmlError::QuickXml {
                    position: reader.buffer_position(),
                    reason: "end tag with no matching open element".to_string(),
                })?;
                scope.pop();
                push_child(&mut stack, &mut root, finished)?;
            }
            Event::Text(t) => {
                // Unescape entities (&amp; &lt; ...) — the xmltree writer will
                // re-escape on output, so the stored value must be decoded.
                let text = t.unescape().map_err(|e| WordXmlError::QuickXml {
                    position: reader.buffer_position(),
                    reason: format!("text unescape failed: {e}"),
                })?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XMLNode::Text(text.into_owned()));
                }
                // Top-level text (outside the root) is whitespace between the
                // XML declaration and the root; xml-rs dropped it, so do we.
            }
            Event::CData(c) => {
                let decoded = c.decode().map_err(|e| WordXmlError::QuickXml {
                    position: reader.buffer_position(),
                    reason: format!("CDATA decode failed: {e}"),
                })?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XMLNode::CData(decoded.into_owned()));
                }
            }
            Event::Comment(c) => {
                // Comment content is literal: decode bytes, do NOT unescape.
                let decoded =
                    reader
                        .decoder()
                        .decode(c.as_ref())
                        .map_err(|e| WordXmlError::QuickXml {
                            position: reader.buffer_position(),
                            reason: format!("comment decode failed: {e}"),
                        })?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XMLNode::Comment(decoded.into_owned()));
                }
            }
            Event::PI(pi) => {
                // Processing instruction: split into (target, data). xmltree
                // stores them as (name, Option<data>).
                let raw =
                    reader
                        .decoder()
                        .decode(pi.as_ref())
                        .map_err(|e| WordXmlError::QuickXml {
                            position: reader.buffer_position(),
                            reason: format!("processing-instruction decode failed: {e}"),
                        })?;
                let (target, data) = match raw.split_once(char::is_whitespace) {
                    Some((t, d)) => (t.to_string(), Some(d.trim_start().to_string())),
                    None => (raw.into_owned(), None),
                };
                if let Some(parent) = stack.last_mut() {
                    parent
                        .children
                        .push(XMLNode::ProcessingInstruction(target, data));
                }
            }
            Event::DocType(_) => {
                // Reject DTDs outright: defense against entity-expansion attacks.
                return Err(WordXmlError::DoctypeRejected);
            }
            Event::Decl(_) => {
                // XML declaration (<?xml ...?>): no tree node, like xml-rs.
            }
            Event::Eof => break,
        }

        buf.clear();
    }

    if !stack.is_empty() {
        return Err(WordXmlError::QuickXml {
            position: reader.buffer_position(),
            reason: format!("{} element(s) left unclosed at EOF", stack.len()),
        });
    }

    root.ok_or(WordXmlError::NoRootElement)
}

/// Attach a finished element to its parent, or record it as the root if the
/// stack is empty. Two root elements is a malformed document.
fn push_child(
    stack: &mut [Element],
    root: &mut Option<Element>,
    element: Element,
) -> Result<(), WordXmlError> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(XMLNode::Element(element));
        Ok(())
    } else if root.is_none() {
        *root = Some(element);
        Ok(())
    } else {
        Err(WordXmlError::QuickXml {
            position: 0,
            reason: "document has more than one root element".to_string(),
        })
    }
}

/// Error returned by `for_each_body_child`: either a structural/XML failure from
/// the streaming parse, or an error bubbled up from the per-child consumer.
pub enum BodyStreamError<E> {
    Xml(WordXmlError),
    Consumer(E),
}

/// Stream `document.xml` once and invoke `consume` for each direct child of
/// `<w:body>`, handing it a freshly materialized `xmltree::Element` subtree for
/// that one child only — which is dropped before the next child is built.
///
/// This is the Rung-6 memory fix: instead of materializing the entire body tree
/// up front (peak = O(whole document)), we keep at most one top-level block's
/// subtree live at a time (peak = O(one block) + the IR built so far).
///
/// The subtree handed to `consume` is byte-identical to the corresponding node
/// of the tree that `parse_document_xml_quick` would have built: the same
/// `element_from_start` builder and the same inherited namespace scope (the
/// declarations on `<w:document>` and `<w:body>`) are used, so prefixes inside
/// the block resolve exactly as before.
///
/// The depth guard and DOCTYPE rejection match `parse_document_xml`.
///
/// `consume` receives the zero-based index of the child among ALL `body.children`
/// (element nodes only advance the index here, matching the old `enumerate()`
/// over children where non-element nodes were skipped — see note below).
pub fn for_each_body_child<E>(
    bytes: &[u8],
    mut consume: impl FnMut(usize, &Element) -> Result<(), E>,
) -> Result<(), BodyStreamError<E>> {
    let xml_err = |e: WordXmlError| BodyStreamError::Xml(e);

    let mut reader = Reader::from_reader(bytes);
    {
        let config = reader.config_mut();
        config.trim_text(false);
        config.expand_empty_elements = false;
        config.check_end_names = true;
    }

    // Namespace scope for ancestors of the element currently being processed.
    // While we are scanning at body-child level, this holds the frames for
    // `document` and `body`, so a subtree built from here resolves prefixes the
    // same way the full-tree builder would.
    let mut scope: Vec<NsFrame> = Vec::new();
    // Depth of currently-open elements: 0 before root, 1 inside <w:document>,
    // 2 inside <w:body>. Body children open at depth 2 -> their subtree.
    let mut depth: usize = 0;
    let mut seen_document = false;
    let mut body_count = 0usize;
    let mut in_body = false;
    // Index of the next body child node. The old path iterated
    // `body.children.iter().enumerate()`, so the index counts ALL child nodes of
    // `<w:body>` — text, CDATA, comments, PIs AND elements — even though only
    // element children are processed. That index is load-bearing: body-level
    // `w:sdt` opaque blocks embed it as `body_index:{i}` / `body_item_{i}`. So we
    // advance `child_index` on EVERY direct child node of body, matching the
    // tree's `children` vector exactly.
    let mut child_index: usize = 0;

    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| {
            xml_err(WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("{e}"),
            })
        })?;

        match event {
            Event::Start(ref start) => {
                let (element, frame) =
                    element_from_start(start, &reader, &scope).map_err(xml_err)?;

                if depth == 0 {
                    if !is_w_tag(&element, "document") {
                        return Err(xml_err(WordXmlError::MissingDocument));
                    }
                    seen_document = true;
                } else if depth == 1 && is_w_tag(&element, "body") {
                    body_count += 1;
                    if body_count > 1 {
                        return Err(xml_err(WordXmlError::MultipleBody(body_count)));
                    }
                    in_body = true;
                } else if in_body && depth == 2 {
                    // A body child opens here. Build its full subtree, carrying
                    // the inherited scope (document + body declarations), then
                    // hand it to the consumer and drop it.
                    scope.push(frame);
                    let subtree =
                        build_subtree_from_open(&mut reader, &mut buf, element, &mut scope)
                            .map_err(xml_err)?;
                    consume(child_index, &subtree).map_err(BodyStreamError::Consumer)?;
                    child_index += 1;
                    // build_subtree_from_open consumed through the child's End
                    // event and popped its frame; we remain at body level.
                    continue;
                }

                scope.push(frame);
                depth += 1;
                if depth > MAX_XML_ELEMENT_DEPTH {
                    return Err(xml_err(WordXmlError::XmlDepthExceeded {
                        limit: MAX_XML_ELEMENT_DEPTH,
                        depth,
                    }));
                }
            }
            Event::Empty(ref start) => {
                let (element, _frame) =
                    element_from_start(start, &reader, &scope).map_err(xml_err)?;
                if depth == 0 {
                    // An empty <w:document/> is malformed for our purposes
                    // (no body) but matches MissingBody after the loop.
                    if !is_w_tag(&element, "document") {
                        return Err(xml_err(WordXmlError::MissingDocument));
                    }
                    seen_document = true;
                } else if in_body && depth == 2 {
                    // Self-closing body child (e.g. <w:sectPr/> or a bookmark
                    // marker). Hand the single element to the consumer.
                    consume(child_index, &element).map_err(BodyStreamError::Consumer)?;
                    child_index += 1;
                }
                // Empty elements at other depths carry no body children.
            }
            Event::End(_) => {
                if in_body && depth == 2 {
                    // Closing </w:body>.
                    in_body = false;
                }
                scope.pop();
                depth = depth.saturating_sub(1);
            }
            Event::DocType(_) => {
                return Err(xml_err(WordXmlError::DoctypeRejected));
            }
            Event::Eof => break,
            // Text / CData / Comment / PI at body level are non-element children
            // of `<w:body>`: the old path skipped processing them but they still
            // occupy a slot in `body.children`, so they advance the child index.
            // (Decl never appears inside the body.)
            Event::Text(_) | Event::CData(_) | Event::Comment(_) | Event::PI(_)
                if in_body && depth == 2 =>
            {
                child_index += 1;
            }
            _ => {}
        }
        buf.clear();
    }

    if !seen_document {
        return Err(xml_err(WordXmlError::MissingDocument));
    }
    if body_count == 0 {
        return Err(xml_err(WordXmlError::MissingBody));
    }
    Ok(())
}

/// Build one element subtree, given its already-built open element (its own
/// `NsFrame` must already be pushed onto `scope`). Consumes events from the
/// reader through the matching End event, pops the element's frame, and returns
/// the finished element. The reader is left positioned after the End event.
fn build_subtree_from_open(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    open: Element,
    scope: &mut Vec<NsFrame>,
) -> Result<Element, WordXmlError> {
    // Local stack rooted at `open`; `scope` already has `open`'s frame on top.
    let mut stack: Vec<Element> = vec![open];

    loop {
        buf.clear();
        let event = reader
            .read_event_into(buf)
            .map_err(|e| WordXmlError::QuickXml {
                position: reader.buffer_position(),
                reason: format!("{e}"),
            })?;
        match event {
            Event::Start(ref start) => {
                if stack.len() + scope.len() > MAX_XML_ELEMENT_DEPTH {
                    return Err(WordXmlError::XmlDepthExceeded {
                        limit: MAX_XML_ELEMENT_DEPTH,
                        depth: stack.len() + scope.len(),
                    });
                }
                let (element, frame) = element_from_start(start, reader, scope)?;
                stack.push(element);
                scope.push(frame);
            }
            Event::Empty(ref start) => {
                let (element, _frame) = element_from_start(start, reader, scope)?;
                let parent = stack
                    .last_mut()
                    .expect("subtree stack never empties before its root closes");
                parent.children.push(XMLNode::Element(element));
            }
            Event::End(_) => {
                let finished = stack.pop().ok_or_else(|| WordXmlError::QuickXml {
                    position: reader.buffer_position(),
                    reason: "end tag with no matching open element".to_string(),
                })?;
                scope.pop();
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XMLNode::Element(finished));
                } else {
                    // Closed the subtree root.
                    return Ok(finished);
                }
            }
            Event::Text(t) => {
                let text = t.unescape().map_err(|e| WordXmlError::QuickXml {
                    position: reader.buffer_position(),
                    reason: format!("text unescape failed: {e}"),
                })?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XMLNode::Text(text.into_owned()));
                }
            }
            Event::CData(c) => {
                let decoded = c.decode().map_err(|e| WordXmlError::QuickXml {
                    position: reader.buffer_position(),
                    reason: format!("CDATA decode failed: {e}"),
                })?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XMLNode::CData(decoded.into_owned()));
                }
            }
            Event::Comment(c) => {
                let decoded =
                    reader
                        .decoder()
                        .decode(c.as_ref())
                        .map_err(|e| WordXmlError::QuickXml {
                            position: reader.buffer_position(),
                            reason: format!("comment decode failed: {e}"),
                        })?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XMLNode::Comment(decoded.into_owned()));
                }
            }
            Event::PI(pi) => {
                let raw =
                    reader
                        .decoder()
                        .decode(pi.as_ref())
                        .map_err(|e| WordXmlError::QuickXml {
                            position: reader.buffer_position(),
                            reason: format!("processing-instruction decode failed: {e}"),
                        })?;
                let (target, data) = match raw.split_once(char::is_whitespace) {
                    Some((t, d)) => (t.to_string(), Some(d.trim_start().to_string())),
                    None => (raw.into_owned(), None),
                };
                if let Some(parent) = stack.last_mut() {
                    parent
                        .children
                        .push(XMLNode::ProcessingInstruction(target, data));
                }
            }
            Event::DocType(_) => return Err(WordXmlError::DoctypeRejected),
            Event::Decl(_) => {}
            Event::Eof => {
                return Err(WordXmlError::QuickXml {
                    position: reader.buffer_position(),
                    reason: "unexpected EOF inside body child subtree".to_string(),
                });
            }
        }
    }
}

fn ensure_xml_depth_within_limit(bytes: &[u8], limit: usize) -> Result<(), WordXmlError> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    let mut buf = Vec::new();
    let mut depth = 0usize;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(_)) => {
                depth += 1;
                if depth > limit {
                    return Err(WordXmlError::XmlDepthExceeded { limit, depth });
                }
            }
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Empty(_)) => {
                let empty_depth = depth + 1;
                if empty_depth > limit {
                    return Err(WordXmlError::XmlDepthExceeded {
                        limit,
                        depth: empty_depth,
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(())
}

pub fn write_document_xml(element: &Element) -> Result<Vec<u8>, WordXmlError> {
    let mut out = Vec::new();
    element.write(&mut out).map_err(WordXmlError::XmlWrite)?;
    Ok(out)
}

const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

// =============================================================================
// General namespace fixup pass
//
// After serialization, opaque content (drawings, objects, fields) may use
// namespace prefixes that were inherited from ancestor elements in the original
// document but are not declared in the new tree. This pass walks the entire
// tree, collects all used prefixes, and ensures each one is declared on the
// root element with the correct URI.
//
// It also updates `mc:Ignorable` to list any extension namespace prefixes
// so Word doesn't reject the file.
// =============================================================================

/// Walks the XML tree rooted at `root`, collects all namespace prefixes used
/// in element and attribute names, and ensures each has a corresponding
/// `xmlns:{prefix}` declaration on the root element.
///
/// For any undeclared prefix that appears in `KNOWN_OOXML_NAMESPACES`, the
/// declaration is added to the root. Unknown prefixes are logged as warnings.
///
/// After ensuring declarations, this also updates `mc:Ignorable` on the root
/// to include any extension namespace prefixes that are used.
pub fn ensure_all_used_namespaces(root: &mut Element) {
    let mut used_prefixes: HashSet<String> = HashSet::new();
    collect_used_prefixes_iterative(root, &mut used_prefixes);

    // Build O(1) lookup maps from the static tables.
    let known_ns_map: HashMap<&str, (&str, &str)> = KNOWN_OOXML_NAMESPACES
        .iter()
        .map(|(p, u)| (*p, (*p, *u)))
        .collect();
    let core_uris: HashSet<&str> = CORE_NAMESPACE_URIS.iter().copied().collect();

    // Build the set of prefixes already declared on the root.
    let declared: HashSet<String> = root
        .namespaces
        .as_ref()
        .map(|ns| ns.into_iter().map(|(p, _)| p.to_string()).collect())
        .unwrap_or_default();

    let ns = root.namespaces.get_or_insert_with(Namespace::empty);

    // Track which extension prefixes need mc:Ignorable.
    let mut extension_prefixes: Vec<&str> = Vec::new();

    // Iterate prefixes in a stable (sorted) order. The declarations added to
    // `ns` are re-sorted by the BTreeMap-backed Namespace regardless, but the
    // ORDER in which extension prefixes are pushed below is observable: it
    // becomes the token order of the `mc:Ignorable` attribute value. Sorting
    // here keeps that value byte-identical across processes rather than leaking
    // the HashSet's per-process iteration order onto the wire (see H1).
    let mut used_prefixes_sorted: Vec<&String> = used_prefixes.iter().collect();
    used_prefixes_sorted.sort();

    for prefix in used_prefixes_sorted {
        // Skip empty prefix, xml, xmlns — always implicitly declared.
        if prefix.is_empty() || prefix == "xml" || prefix == "xmlns" {
            continue;
        }

        if declared.contains(prefix.as_str()) {
            // Already declared — check if it's an extension namespace for mc:Ignorable.
            if let Some(uri) = ns.get(prefix)
                && !core_uris.contains(uri)
                && let Some((known_prefix, _)) = known_ns_map.get(prefix.as_str())
            {
                extension_prefixes.push(known_prefix);
            }
            // If not in known map but declared with a non-core URI, it's still
            // an extension — but we can't get a 'static &str for it, and the
            // document already has the declaration. We'll check again below
            // using the attribute value approach.
            continue;
        }

        // Not declared on root — look up in known namespace map.
        if let Some((known_prefix, uri)) = known_ns_map.get(prefix.as_str()) {
            ns.put(*known_prefix, *uri);
            if !core_uris.contains(uri) {
                extension_prefixes.push(known_prefix);
            }
        } else {
            tracing::warn!(
                prefix = prefix.as_str(),
                "namespace prefix used in XML tree but not in known OOXML namespace map — \
                 skipping declaration (may be a vendor extension)"
            );
        }
    }

    // Update mc:Ignorable to include all extension namespace prefixes.
    ensure_mc_ignorable_for_prefixes(root, &extension_prefixes);
}

/// Ensures that `mc:Ignorable` on the root element lists all the given
/// extension namespace prefixes.
fn ensure_mc_ignorable_for_prefixes(root: &mut Element, extension_prefixes: &[&str]) {
    if extension_prefixes.is_empty() {
        return;
    }

    // Ensure mc namespace itself is declared.
    let ns = root.namespaces.get_or_insert_with(Namespace::empty);
    ns.put("mc", MC_NS);

    let current = attr_get(root, "mc:Ignorable").cloned().unwrap_or_default();
    let current_set: HashSet<&str> = current.split_whitespace().collect();

    let mut updated = current.clone();
    for prefix in extension_prefixes {
        if prefix.is_empty() {
            continue;
        }
        if !current_set.contains(prefix) {
            if updated.is_empty() {
                updated = prefix.to_string();
            } else {
                updated = format!("{updated} {prefix}");
            }
        }
    }

    if updated != current {
        attr_set(root, "mc:Ignorable", updated);
    }
}

/// Iteratively collect all namespace prefixes used in element names and
/// attribute names throughout the tree. Uses an explicit stack to avoid
/// deep recursion overhead and stack overflow risk on very deep trees.
fn collect_used_prefixes_iterative(root: &Element, out: &mut HashSet<String>) {
    let mut stack: Vec<&Element> = vec![root];
    while let Some(element) = stack.pop() {
        // Element prefix: from the Element's `prefix` field, or embedded in the name.
        if let Some(ref prefix) = element.prefix {
            out.insert(prefix.clone());
        } else if let Some(prefix) = extract_prefix(&element.name) {
            out.insert(prefix.to_string());
        }

        // Attribute prefixes.
        for (attr_name, _) in &element.attributes {
            if let Some(ref prefix) = attr_name.prefix {
                out.insert(prefix.clone());
            } else if let Some(prefix) = extract_prefix(&attr_name.local_name) {
                out.insert(prefix.to_string());
            }
        }

        // Push children onto the stack.
        for child in &element.children {
            if let XMLNode::Element(child_el) = child {
                stack.push(child_el);
            }
        }
    }
}

/// Extract the namespace prefix from a qualified name like "wps:bodyPr".
fn extract_prefix(name: &str) -> Option<&str> {
    match name.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => Some(prefix),
        _ => None,
    }
}

pub fn body_element_mut(root: &mut Element) -> Result<&mut Element, WordXmlError> {
    if !is_w_tag(root, "document") {
        return Err(WordXmlError::MissingDocument);
    }
    let body_count = count_w_children(root, "body");
    if body_count > 1 {
        return Err(WordXmlError::MultipleBody(body_count));
    }
    find_w_child_mut(root, "body").ok_or(WordXmlError::MissingBody)
}

pub fn body_element(root: &Element) -> Result<&Element, WordXmlError> {
    if !is_w_tag(root, "document") {
        return Err(WordXmlError::MissingDocument);
    }
    let body_count = count_w_children(root, "body");
    if body_count > 1 {
        return Err(WordXmlError::MultipleBody(body_count));
    }
    find_w_child(root, "body").ok_or(WordXmlError::MissingBody)
}

/// Parse raw XML bytes produced by the serializer's `serialize_element`.
///
/// The serializer declares all used namespace prefixes on the root element,
/// so the raw bytes are self-contained. This wrapper adds a synthetic root
/// that *also* declares every prefix in `KNOWN_OOXML_NAMESPACES` — supplemental,
/// to handle legacy raw bytes that may lack declarations. After parsing, the
/// propagated `namespaces` map is cleared so no redundant declarations are
/// emitted when the element is later written.
pub(crate) fn parse_raw_fragment(raw: &[u8]) -> Result<Element, xmltree::ParseError> {
    use std::sync::LazyLock;

    static NS_WRAPPER_PREFIX: LazyLock<Vec<u8>> = LazyLock::new(|| {
        let mut s = String::from("<_ns_root");
        for (prefix, uri) in KNOWN_OOXML_NAMESPACES {
            s.push_str(&format!(" xmlns:{prefix}=\"{uri}\""));
        }
        s.push('>');
        s.into_bytes()
    });
    static NS_WRAPPER_SUFFIX: &[u8] = b"</_ns_root>";

    let content = if raw.starts_with(b"<?xml") {
        match raw.iter().position(|&b| b == b'>') {
            Some(pos) => &raw[pos + 1..],
            None => raw,
        }
    } else {
        raw
    };

    let mut wrapped =
        Vec::with_capacity(NS_WRAPPER_PREFIX.len() + content.len() + NS_WRAPPER_SUFFIX.len());
    wrapped.extend_from_slice(&NS_WRAPPER_PREFIX);
    wrapped.extend_from_slice(content);
    wrapped.extend_from_slice(NS_WRAPPER_SUFFIX);

    // Bound nesting depth before the recursive-descent `Element::parse` (and the
    // recursive `strip_ns_decls` pass below) ever touch these bytes. These are
    // untrusted, archive-derived opaque/drawing fragments; unlike the
    // document-XML path (`parse_document_xml`), this fragment path previously had
    // no depth guard, so a crafted deeply-nested fragment could overflow the
    // thread stack — an uncatchable process abort that takes down the whole MCP
    // server. Fail closed with a parse error instead.
    if ensure_xml_depth_within_limit(&wrapped, MAX_XML_ELEMENT_DEPTH).is_err() {
        return Err(xmltree::ParseError::CannotParse);
    }

    // Whitespace-only text nodes are CONTENT here (e.g. an OMML
    // `<m:t xml:space="preserve">  </m:t>` inside an opaque fragment); the
    // default parser config drops them, silently deleting visible characters
    // from byte-preserved opaque content. Mirror the main parser's
    // whitespace-preserving behavior.
    let config = xmltree::ParserConfig::new()
        .whitespace_to_characters(true)
        .cdata_to_characters(true);
    let root = Element::parse_with_config(Cursor::new(&wrapped), config)?;
    match root.children.into_iter().find_map(|c| match c {
        XMLNode::Element(mut el) => {
            let used = crate::word_ir::collect_prefix_uri_bindings(&el);
            strip_ns_decls(&mut el);
            if !used.is_empty() {
                let mut ns = Namespace::empty();
                for (prefix, uri) in &used {
                    ns.put(prefix.as_str(), uri.as_str());
                }
                el.namespaces = Some(ns);
            }
            Some(el)
        }
        _ => None,
    }) {
        Some(el) => Ok(el),
        None => Err(xmltree::ParseError::CannotParse),
    }
}

fn strip_ns_decls(element: &mut Element) {
    element.namespaces = None;
    for child in &mut element.children {
        if let XMLNode::Element(child_el) = child {
            strip_ns_decls(child_el);
        }
    }
}

/// Re-serialize an [`Element`] that was produced by [`parse_raw_fragment`] back
/// into self-contained raw bytes — the inverse of `parse_raw_fragment`.
///
/// Like `word_ir::serialize_element`, it re-declares exactly the namespace
/// prefixes used within the subtree on the root so the bytes round-trip through
/// `parse_raw_fragment` again, and writes no XML document declaration. This is
/// the pair an authoring verb uses to mutate an opaque inline's `raw_xml` in
/// place (e.g. resize a drawing's `wp:extent`) without disturbing any other
/// part of the fragment.
pub(crate) fn serialize_raw_fragment(element: &Element) -> Vec<u8> {
    use xmltree::EmitterConfig;

    let bindings = crate::word_ir::collect_prefix_uri_bindings(element);
    let mut stripped = element.clone();
    strip_ns_decls(&mut stripped);

    if !bindings.is_empty() {
        let mut ns = Namespace::empty();
        for (prefix, uri) in &bindings {
            ns.put(prefix.as_str(), uri.as_str());
        }
        stripped.namespaces = Some(ns);
    }

    let mut buf = Vec::new();
    let config = EmitterConfig::new().write_document_declaration(false);
    // Writing to an in-memory Vec: the only failure mode is the emitter
    // refusing the tree itself (malformed names — a programmer bug, since this
    // tree came from `parse_raw_fragment` or our own builders). Crash with the
    // invariant named rather than store a silently truncated fragment.
    stripped
        .write_with_config(&mut buf, config)
        .expect("re-serializing a parsed opaque fragment must not fail");
    buf
}

pub(crate) fn w_el(local: &str) -> Element {
    let mut element = Element::new(local);
    element.prefix = Some("w".to_string());
    element.namespace = Some(WORD_NS.to_string());
    element
}

/// Check if an element is a Word namespace tag with the given local name.
///
/// Matches when the element has the correct local name AND either:
/// - the `w:` prefix (explicit namespace prefix), or
/// - the Word namespace URI (e.g., default namespace declaration), or
/// - the name contains an embedded `w:` prefix (e.g., `"w:p"` as full name,
///   from constructors/parsers that don't separate prefix from name).
///
/// Elements with no namespace and no `w:` prefix do NOT match — this prevents
/// bare, namespace-less elements from being incorrectly treated as Word elements.
pub(crate) fn is_w_tag(element: &Element, local: &str) -> bool {
    if element.name == local {
        if element.prefix.as_deref() == Some("w") {
            return true;
        }
        return element.namespace.as_deref() == Some(WORD_NS);
    }
    element.name == format!("w:{local}")
}

fn count_w_children(element: &Element, local: &str) -> usize {
    element
        .children
        .iter()
        .filter(|child| matches!(child, XMLNode::Element(el) if is_w_tag(el, local)))
        .count()
}

fn find_w_child<'a>(element: &'a Element, local: &str) -> Option<&'a Element> {
    element.children.iter().find_map(|child| {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => return None,
        };
        if is_w_tag(el, local) { Some(el) } else { None }
    })
}

fn find_w_child_mut<'a>(element: &'a mut Element, local: &str) -> Option<&'a mut Element> {
    element.children.iter_mut().find_map(|child| {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => return None,
        };
        if is_w_tag(el, local) { Some(el) } else { None }
    })
}

/// Creates a `<w:del>` element with revision attributes
pub fn w_del(revision_id: u32, author: &str, date: &str) -> Element {
    let mut del = w_el("del");
    attr_set(&mut del, "w:id", revision_id.to_string());
    attr_set(&mut del, "w:author", author);
    attr_set(&mut del, "w:date", date);
    attr_set(&mut del, "w16du:dateUtc", date);
    del
}

/// Creates a `<w:ins>` element with revision attributes
pub fn w_ins(revision_id: u32, author: &str, date: &str) -> Element {
    let mut ins = w_el("ins");
    attr_set(&mut ins, "w:id", revision_id.to_string());
    attr_set(&mut ins, "w:author", author);
    attr_set(&mut ins, "w:date", date);
    attr_set(&mut ins, "w16du:dateUtc", date);
    ins
}

/// Creates a `<w:moveFrom>` element with revision attributes (tracked move source).
pub fn w_move_from(revision_id: u32, author: &str, date: &str) -> Element {
    let mut el = w_el("moveFrom");
    attr_set(&mut el, "w:id", revision_id.to_string());
    attr_set(&mut el, "w:author", author);
    attr_set(&mut el, "w:date", date);
    attr_set(&mut el, "w16du:dateUtc", date);
    el
}

/// Creates a `<w:moveTo>` element with revision attributes (tracked move destination).
pub fn w_move_to(revision_id: u32, author: &str, date: &str) -> Element {
    let mut el = w_el("moveTo");
    attr_set(&mut el, "w:id", revision_id.to_string());
    attr_set(&mut el, "w:author", author);
    attr_set(&mut el, "w:date", date);
    attr_set(&mut el, "w16du:dateUtc", date);
    el
}

/// Creates a `<w:moveFromRangeStart>` bookmark element.
pub fn w_move_from_range_start(bookmark_id: u32, name: &str, author: &str, date: &str) -> Element {
    let mut el = w_el("moveFromRangeStart");
    attr_set(&mut el, "w:id", bookmark_id.to_string());
    attr_set(&mut el, "w:name", name);
    attr_set(&mut el, "w:author", author);
    attr_set(&mut el, "w:date", date);
    el
}

/// Creates a `<w:moveFromRangeEnd>` bookmark element.
pub fn w_move_from_range_end(bookmark_id: u32) -> Element {
    let mut el = w_el("moveFromRangeEnd");
    attr_set(&mut el, "w:id", bookmark_id.to_string());
    el
}

/// Creates a `<w:moveToRangeStart>` bookmark element.
pub fn w_move_to_range_start(bookmark_id: u32, name: &str, author: &str, date: &str) -> Element {
    let mut el = w_el("moveToRangeStart");
    attr_set(&mut el, "w:id", bookmark_id.to_string());
    attr_set(&mut el, "w:name", name);
    attr_set(&mut el, "w:author", author);
    attr_set(&mut el, "w:date", date);
    el
}

/// Creates a `<w:moveToRangeEnd>` bookmark element.
pub fn w_move_to_range_end(bookmark_id: u32) -> Element {
    let mut el = w_el("moveToRangeEnd");
    attr_set(&mut el, "w:id", bookmark_id.to_string());
    el
}

/// Creates a `<w:cellIns>` element with revision attributes (tracked cell insertion)
pub fn w_cell_ins(revision_id: u32, author: &str, date: &str) -> Element {
    let mut cell_ins = w_el("cellIns");
    attr_set(&mut cell_ins, "w:id", revision_id.to_string());
    attr_set(&mut cell_ins, "w:author", author);
    attr_set(&mut cell_ins, "w:date", date);
    attr_set(&mut cell_ins, "w16du:dateUtc", date);
    cell_ins
}

/// Creates a `<w:cellDel>` element with revision attributes (tracked cell deletion)
pub fn w_cell_del(revision_id: u32, author: &str, date: &str) -> Element {
    let mut cell_del = w_el("cellDel");
    attr_set(&mut cell_del, "w:id", revision_id.to_string());
    attr_set(&mut cell_del, "w:author", author);
    attr_set(&mut cell_del, "w:date", date);
    attr_set(&mut cell_del, "w16du:dateUtc", date);
    cell_del
}

/// Creates a `<w:delText>` element (used inside deleted runs)
pub fn w_del_text(text: &str) -> Element {
    let mut del_text = w_el("delText");
    if text.starts_with(' ') || text.ends_with(' ') {
        attr_set(&mut del_text, "xml:space", "preserve");
    }
    del_text.children.push(XMLNode::Text(text.to_string()));
    del_text
}

/// Locate (creating as needed) the `<w:pPr><w:rPr>` of `paragraph` and append
/// `marker` — the paragraph-mark tracked-change element (`w:ins`/`w:del`/
/// `w:moveTo`/`w:moveFrom`). Shared by the four `ensure_ppr_rpr_*` wrappers.
fn append_ppr_rpr_marker(paragraph: &mut Element, marker: Element) {
    // Find or create <w:pPr> (at position 0, before all other children).
    let ppr_idx = paragraph
        .children
        .iter()
        .position(|c| matches!(c, XMLNode::Element(el) if is_w_tag(el, "pPr")))
        .unwrap_or_else(|| {
            paragraph.children.insert(0, XMLNode::Element(w_el("pPr")));
            0
        });
    let ppr = match &mut paragraph.children[ppr_idx] {
        XMLNode::Element(el) => el,
        _ => unreachable!(),
    };

    // Find or create <w:rPr> inside pPr. CT_PPr order:
    // (CT_PPrBase children..., rPr?, sectPr?, pPrChange?) — insert before
    // sectPr/pPrChange.
    let rpr_idx = ppr
        .children
        .iter()
        .position(|c| matches!(c, XMLNode::Element(el) if is_w_tag(el, "rPr")))
        .unwrap_or_else(|| {
            let insert_pos = ppr
                .children
                .iter()
                .position(|c| {
                    matches!(c, XMLNode::Element(el) if is_w_tag(el, "sectPr") || is_w_tag(el, "pPrChange"))
                })
                .unwrap_or(ppr.children.len());
            ppr.children
                .insert(insert_pos, XMLNode::Element(w_el("rPr")));
            insert_pos
        });
    let rpr = match &mut ppr.children[rpr_idx] {
        XMLNode::Element(el) => el,
        _ => unreachable!(),
    };

    rpr.children.push(XMLNode::Element(marker));
}

/// Marks the paragraph mark as inserted by adding `<w:ins>` inside `<w:pPr><w:rPr>`.
///
/// OOXML requires this in addition to wrapping runs in `<w:ins>`. Without it,
/// empty inserted paragraphs have no visible tracked insertion, and accept/reject
/// semantics may break.
pub fn ensure_ppr_rpr_ins(paragraph: &mut Element, rev_id: u32, author: &str, date: &str) {
    append_ppr_rpr_marker(paragraph, w_ins(rev_id, author, date));
}

/// Marks the paragraph mark as deleted by adding `<w:del>` inside `<w:pPr><w:rPr>`.
///
/// OOXML requires this for proper accept/reject of paragraph-level deletions.
pub fn ensure_ppr_rpr_del(paragraph: &mut Element, rev_id: u32, author: &str, date: &str) {
    append_ppr_rpr_marker(paragraph, w_del(rev_id, author, date));
}

/// Marks the paragraph mark as a MOVE DESTINATION by adding `<w:moveTo>` inside
/// `<w:pPr><w:rPr>`. This is the moved-paragraph twin of `ensure_ppr_rpr_ins`:
/// when a paragraph's runs are moved (wrapped in `w:moveTo`), its terminating
/// pilcrow must carry `w:moveTo` too — NOT a plain `w:ins` — so Word resolves
/// the whole moved paragraph as one move (real Word emits exactly this shape).
/// A plain `w:ins` on a moveTo destination's pilcrow is an INDEPENDENT
/// paragraph-mark insertion Word rejects on its own, merging the paragraph with
/// the next (§17.13.5.20 CT_ParaRPr permits moveTo/moveFrom on the mark).
pub fn ensure_ppr_rpr_move_to(paragraph: &mut Element, rev_id: u32, author: &str, date: &str) {
    append_ppr_rpr_marker(paragraph, w_move_to(rev_id, author, date));
}

/// Marks the paragraph mark as a MOVE SOURCE by adding `<w:moveFrom>` inside
/// `<w:pPr><w:rPr>`. The moveFrom-shadow twin of `ensure_ppr_rpr_del`; see
/// `ensure_ppr_rpr_move_to`.
pub fn ensure_ppr_rpr_move_from(paragraph: &mut Element, rev_id: u32, author: &str, date: &str) {
    append_ppr_rpr_marker(paragraph, w_move_from(rev_id, author, date));
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use xmltree::ParserConfig;

    #[test]
    fn empty_or_whitespace_xml_covers_only_contentless_bytes() {
        // Word emits these shapes for an empty running head — tolerated.
        assert!(is_empty_or_whitespace_xml(b""));
        assert!(is_empty_or_whitespace_xml(b"   \r\n\t "));
        assert!(is_empty_or_whitespace_xml(&[0xEF, 0xBB, 0xBF])); // lone UTF-8 BOM
        assert!(is_empty_or_whitespace_xml(&[0xEF, 0xBB, 0xBF, b'\n', b' ']));
        // Anything with real content is NOT empty — including a bare XML
        // declaration with no root (a truncated/malformed part, not an empty
        // running head), a valid root, and plain garbage.
        assert!(!is_empty_or_whitespace_xml(b"<?xml version=\"1.0\"?>"));
        assert!(!is_empty_or_whitespace_xml(b"<w:hdr/>"));
        assert!(!is_empty_or_whitespace_xml(b"x"));
    }

    fn parse_xml(xml: &str) -> Element {
        // Exercise the Approach-A quick-xml builder directly (this is the path
        // production import now uses).
        parse_document_xml_quick(xml.as_bytes()).expect("test XML should parse")
    }

    fn nested_document_xml(depth: usize) -> Vec<u8> {
        let mut xml = String::from(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
        );
        for _ in 0..depth {
            xml.push_str("<w:tbl><w:tr><w:tc>");
        }
        xml.push_str("<w:p/>");
        for _ in 0..depth {
            xml.push_str("</w:tc></w:tr></w:tbl>");
        }
        xml.push_str("</w:body></w:document>");
        xml.into_bytes()
    }

    #[test]
    fn xml_depth_guard_allows_depth_within_limit() {
        let xml = nested_document_xml(16);
        assert!(ensure_xml_depth_within_limit(&xml, 64).is_ok());
    }

    #[test]
    fn xml_depth_guard_rejects_excessive_nesting() {
        let xml = nested_document_xml(16);
        match ensure_xml_depth_within_limit(&xml, 32) {
            Err(WordXmlError::XmlDepthExceeded { limit, depth }) => {
                assert_eq!(limit, 32);
                assert!(depth > limit);
            }
            other => panic!("expected XmlDepthExceeded, got {other:?}"),
        }
    }

    #[test]
    fn parse_raw_fragment_rejects_overdeep_nesting_without_aborting() {
        // A crafted, deeply-nested opaque fragment must fail closed with a parse
        // error rather than recursing into `Element::parse` and overflowing the
        // stack (an uncatchable process abort). Nest past MAX_XML_ELEMENT_DEPTH.
        let depth = MAX_XML_ELEMENT_DEPTH + 50;
        let mut raw = String::new();
        for _ in 0..depth {
            raw.push_str("<w:p>");
        }
        for _ in 0..depth {
            raw.push_str("</w:p>");
        }
        assert!(
            parse_raw_fragment(raw.as_bytes()).is_err(),
            "over-deep fragment must be rejected, not parsed/aborted"
        );
    }

    #[test]
    fn parse_raw_fragment_accepts_shallow_nesting() {
        // A small, legitimately-nested fragment still round-trips through the
        // guard unharmed.
        let raw = r#"<w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#;
        assert!(parse_raw_fragment(raw.as_bytes()).is_ok());
    }

    #[test]
    fn ensure_all_used_namespaces_adds_missing_wps_declaration() {
        // Simulate a document root that uses wps: in a child element
        // but doesn't declare it.
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <w:body>
                <w:p>
                    <w:r>
                        <wps:bodyPr xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"/>
                    </w:r>
                </w:p>
            </w:body>
        </w:document>"#;
        let mut root = parse_xml(xml);

        // Before fixup: wps is declared on the child, but we want it on the root.
        ensure_all_used_namespaces(&mut root);

        // The wps namespace should now be declared on the root element.
        let ns = root.namespaces.as_ref().expect("namespaces should exist");
        assert_eq!(
            ns.get("wps"),
            Some("http://schemas.microsoft.com/office/word/2010/wordprocessingShape"),
            "wps namespace should be declared on root"
        );
    }

    #[test]
    fn ensure_all_used_namespaces_adds_missing_undeclared_prefix() {
        // Simulate opaque content that uses a prefix without any declaration
        // (as happens when the declaration was on an ancestor in the original
        // document that isn't part of our tree).
        let mut root = Element::new("document");
        root.prefix = Some("w".to_string());
        root.namespace = Some(WORD_NS.to_string());
        let ns = root.namespaces.get_or_insert_with(Namespace::empty);
        ns.put("w", WORD_NS);

        // Add a child with a14: prefix but no declaration.
        let mut child = Element::new("imgLayer");
        child.prefix = Some("a14".to_string());
        // No namespace URI set — simulates a reconstructed element.
        root.children.push(XMLNode::Element(child));

        ensure_all_used_namespaces(&mut root);

        let ns = root.namespaces.as_ref().unwrap();
        assert_eq!(
            ns.get("a14"),
            Some("http://schemas.microsoft.com/office/drawing/2010/main"),
            "a14 should be declared on root after fixup"
        );
    }

    #[test]
    fn ensure_all_used_namespaces_declares_w16du_and_w14() {
        // Build the tree programmatically to simulate opaque content that uses
        // w14: and w16du: prefixes without root-level declarations — this is
        // exactly what happens when opaque widgets are re-emitted.
        let mut root = Element::new("document");
        root.prefix = Some("w".to_string());
        root.namespace = Some(WORD_NS.to_string());
        let ns = root.namespaces.get_or_insert_with(Namespace::empty);
        ns.put("w", WORD_NS);

        // A child paragraph with w14:paraId attribute.
        let mut para = Element::new("p");
        para.prefix = Some("w".to_string());
        para.namespace = Some(WORD_NS.to_string());
        let attr_name = xmltree::AttributeName {
            local_name: "paraId".to_string(),
            namespace: None,
            prefix: Some("w14".to_string()),
        };
        para.attributes.insert(attr_name, "AABBCCDD".to_string());

        // A w:ins child with w16du:dateUtc attribute (w_ins adds it).
        let ins = w_ins(1, "test", "2025-01-01");
        para.children.push(XMLNode::Element(ins));
        root.children.push(XMLNode::Element(para));

        ensure_all_used_namespaces(&mut root);

        let ns = root.namespaces.as_ref().unwrap();
        assert_eq!(
            ns.get("w14"),
            Some("http://schemas.microsoft.com/office/word/2010/wordml"),
            "w14 should be declared"
        );
        assert_eq!(
            ns.get("w16du"),
            Some("http://schemas.microsoft.com/office/word/2023/wordml/word16du"),
            "w16du should be declared"
        );
    }

    #[test]
    fn ensure_all_used_namespaces_updates_mc_ignorable() {
        // Build programmatically: root with w: + mc:, child with w16du: attribute.
        let mut root = Element::new("document");
        root.prefix = Some("w".to_string());
        root.namespace = Some(WORD_NS.to_string());
        let ns = root.namespaces.get_or_insert_with(Namespace::empty);
        ns.put("w", WORD_NS);
        ns.put("mc", MC_NS);

        let ins = w_ins(1, "test", "2025-01-01"); // adds w16du:dateUtc
        let mut para = w_el("p");
        para.children.push(XMLNode::Element(ins));
        root.children.push(XMLNode::Element(para));

        ensure_all_used_namespaces(&mut root);

        // mc:Ignorable should list w16du (extension namespace).
        let ignorable = attr_get(&root, "mc:Ignorable").expect("mc:Ignorable should be set");
        assert!(
            ignorable.split_whitespace().any(|p| p == "w16du"),
            "mc:Ignorable should include w16du, got: {ignorable}"
        );
    }

    #[test]
    fn ensure_all_used_namespaces_does_not_add_core_to_mc_ignorable() {
        // Core namespaces (w, r, m, a, etc.) should NOT appear in mc:Ignorable.
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <w:body>
                <w:p><w:r><w:t>hello</w:t></w:r></w:p>
            </w:body>
        </w:document>"#;
        let mut root = parse_xml(xml);
        ensure_all_used_namespaces(&mut root);

        // mc:Ignorable should not be set (no extension namespaces used).
        let ignorable = attr_get(&root, "mc:Ignorable").cloned().unwrap_or_default();
        assert!(
            ignorable.is_empty(),
            "mc:Ignorable should be empty for core-only namespaces, got: {ignorable}"
        );
    }

    #[test]
    fn ensure_all_used_namespaces_preserves_existing_mc_ignorable() {
        // Build programmatically: root already has mc:Ignorable="w15",
        // and a child uses w16du: (so it should be added, not replacing w15).
        let mut root = Element::new("document");
        root.prefix = Some("w".to_string());
        root.namespace = Some(WORD_NS.to_string());
        let ns = root.namespaces.get_or_insert_with(Namespace::empty);
        ns.put("w", WORD_NS);
        ns.put("mc", MC_NS);
        ns.put(
            "w15",
            "http://schemas.microsoft.com/office/word/2012/wordml",
        );
        attr_set(&mut root, "mc:Ignorable", "w15");

        // Add a child that uses w15: (so it shows up in used prefixes).
        let mut w15_el = Element::new("color");
        w15_el.prefix = Some("w15".to_string());
        root.children.push(XMLNode::Element(w15_el));

        let ins = w_ins(1, "test", "2025-01-01"); // adds w16du:dateUtc
        let mut para = w_el("p");
        para.children.push(XMLNode::Element(ins));
        root.children.push(XMLNode::Element(para));

        ensure_all_used_namespaces(&mut root);

        let ignorable = attr_get(&root, "mc:Ignorable").expect("mc:Ignorable should be set");
        let prefixes: HashSet<&str> = ignorable.split_whitespace().collect();
        assert!(
            prefixes.contains("w15"),
            "existing w15 should be preserved in mc:Ignorable"
        );
        assert!(
            prefixes.contains("w16du"),
            "w16du should be added to mc:Ignorable"
        );
    }

    #[test]
    fn ensure_all_used_namespaces_handles_attribute_prefixes() {
        // Test that attribute prefixes (like r:id on drawings) are detected.
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <w:body>
                <w:p>
                    <w:r>
                        <a:blip r:embed="rId1" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"/>
                    </w:r>
                </w:p>
            </w:body>
        </w:document>"#;
        let mut root = parse_xml(xml);
        ensure_all_used_namespaces(&mut root);

        let ns = root.namespaces.as_ref().unwrap();
        // r and a should both be declared on the root.
        assert!(ns.get("r").is_some(), "r namespace should be on root");
        assert!(ns.get("a").is_some(), "a namespace should be on root");
    }

    #[test]
    fn ensure_all_used_namespaces_multiple_drawing_prefixes() {
        // Simulate a document with drawing content using wpg, wps, a14 prefixes.
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <w:body>
                <w:p>
                    <w:r>
                        <wpg:wgp xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup">
                            <wps:wsp xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
                                <a14:imgLayer xmlns:a14="http://schemas.microsoft.com/office/drawing/2010/main"/>
                            </wps:wsp>
                        </wpg:wgp>
                    </w:r>
                </w:p>
            </w:body>
        </w:document>"#;
        let mut root = parse_xml(xml);
        ensure_all_used_namespaces(&mut root);

        let ns = root.namespaces.as_ref().unwrap();
        assert!(ns.get("wpg").is_some(), "wpg should be declared on root");
        assert!(ns.get("wps").is_some(), "wps should be declared on root");
        assert!(ns.get("a14").is_some(), "a14 should be declared on root");
    }

    #[test]
    fn collect_used_prefixes_finds_embedded_prefix_in_name() {
        // Some elements may have prefixes embedded in the name string rather than
        // in the separate prefix field.
        let mut element = Element::new("wps:bodyPr");
        // No prefix field set — the prefix is in the name.
        element.prefix = None;

        let mut used = HashSet::new();
        collect_used_prefixes_iterative(&element, &mut used);
        assert!(used.contains("wps"), "should detect wps from name string");
    }

    #[test]
    fn body_element_rejects_multiple_bodies() {
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body><w:p/></w:body>
            <w:body><w:p/></w:body>
            <w:body><w:p/></w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        match body_element(&root) {
            Err(WordXmlError::MultipleBody(3)) => {} // expected
            other => panic!("expected MultipleBody(3), got {other:?}"),
        }
    }

    #[test]
    fn body_element_accepts_single_body() {
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body><w:p/></w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        assert!(body_element(&root).is_ok());
    }

    // =========================================================================
    // Approach A: quick-xml builder fidelity vs the previous xml-rs path.
    //
    // The contract is "the engine reads/re-serializes the same tree". The
    // strongest check is a differential one: parse the same bytes with both the
    // old xml-rs parser and the new quick-xml builder and assert the resulting
    // serialized output matches. Serialization normalizes the two trees through
    // the same writer, so any field that actually affects output is covered.
    // =========================================================================

    fn parse_xml_rs(bytes: &[u8]) -> Element {
        let config = ParserConfig::new()
            .ignore_comments(false)
            .whitespace_to_characters(true);
        Element::parse_with_config(Cursor::new(bytes), config).expect("xml-rs should parse")
    }

    fn assert_roundtrip_matches(xml: &str) {
        let old = parse_xml_rs(xml.as_bytes());
        let new = parse_document_xml_quick(xml.as_bytes()).expect("quick-xml should parse");

        let old_out = write_document_xml(&old).expect("xml-rs tree serializes");
        let new_out = write_document_xml(&new).expect("quick-xml tree serializes");

        assert_eq!(
            String::from_utf8_lossy(&old_out),
            String::from_utf8_lossy(&new_out),
            "serialized output of quick-xml builder must match xml-rs path"
        );
    }

    #[test]
    fn quick_builder_matches_xml_rs_basic_document() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"#;
        assert_roundtrip_matches(xml);
    }

    #[test]
    fn quick_builder_preserves_whitespace_with_xml_space() {
        // The space inside <w:t xml:space="preserve"> must survive verbatim.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve"> leading and trailing </w:t></w:r></w:p></w:body></w:document>"#;
        let root = parse_document_xml_quick(xml.as_bytes()).unwrap();
        // Find the w:t and check its text node is exactly the original.
        let body = find_w_child(&root, "body").unwrap();
        let p = find_w_child(body, "p").unwrap();
        let r = find_w_child(p, "r").unwrap();
        let t = find_w_child(r, "t").unwrap();
        let text = t
            .children
            .iter()
            .find_map(|c| {
                if let XMLNode::Text(s) = c {
                    Some(s)
                } else {
                    None
                }
            })
            .expect("w:t has text");
        assert_eq!(text, " leading and trailing ");
        assert_roundtrip_matches(xml);
    }

    #[test]
    fn quick_builder_unescapes_text_and_attrs() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>a &amp; b &lt; c &gt; d "e" 'f'</w:t></w:r></w:p></w:body></w:document>"#;
        let root = parse_document_xml_quick(xml.as_bytes()).unwrap();
        let body = find_w_child(&root, "body").unwrap();
        let p = find_w_child(body, "p").unwrap();
        let r = find_w_child(p, "r").unwrap();
        let t = find_w_child(r, "t").unwrap();
        let text = t
            .children
            .iter()
            .find_map(|c| {
                if let XMLNode::Text(s) = c {
                    Some(s)
                } else {
                    None
                }
            })
            .unwrap();
        // Stored value is the DECODED text (the writer re-escapes on output).
        assert_eq!(text, r#"a & b < c > d "e" 'f'"#);
        assert_roundtrip_matches(xml);
    }

    #[test]
    fn quick_builder_attribute_with_entities_roundtrips() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t/></w:r><w:bookmarkStart w:id="1" w:name="a &amp; b &lt; c"/></w:p></w:body></w:document>"#;
        assert_roundtrip_matches(xml);
    }

    #[test]
    fn quick_builder_preserves_attribute_order() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t/></w:r><w:ins w:id="5" w:author="zeta" w:date="2020-01-01T00:00:00Z"/></w:p></w:body></w:document>"#;
        let root = parse_document_xml_quick(xml.as_bytes()).unwrap();
        let body = find_w_child(&root, "body").unwrap();
        let p = find_w_child(body, "p").unwrap();
        let ins = find_w_child(p, "ins").unwrap();
        let order: Vec<&str> = ins
            .attributes
            .keys()
            .map(|k| k.local_name.as_str())
            .collect();
        assert_eq!(order, vec!["id", "author", "date"]);
        assert_roundtrip_matches(xml);
    }

    #[test]
    fn quick_builder_handles_comments() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><!-- a comment with & < > kept literal --><w:p/></w:body></w:document>"#;
        let root = parse_document_xml_quick(xml.as_bytes()).unwrap();
        let body = find_w_child(&root, "body").unwrap();
        let comment = body
            .children
            .iter()
            .find_map(|c| {
                if let XMLNode::Comment(s) = c {
                    Some(s)
                } else {
                    None
                }
            })
            .expect("comment preserved");
        assert_eq!(comment, " a comment with & < > kept literal ");
        assert_roundtrip_matches(xml);
    }

    #[test]
    fn quick_builder_resolves_namespace_uri_for_is_w_tag() {
        // Prefixed form: is_w_tag relies on .namespace being the Word URI.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p/></w:body></w:document>"#;
        let root = parse_document_xml_quick(xml.as_bytes()).unwrap();
        assert!(is_w_tag(&root, "document"));
        assert_eq!(root.namespace.as_deref(), Some(WORD_NS));
        let body = find_w_child(&root, "body").unwrap();
        assert_eq!(body.namespace.as_deref(), Some(WORD_NS));
        assert!(is_w_tag(body, "body"));
    }

    #[test]
    fn quick_builder_handles_default_namespace() {
        // Default namespace (no prefix) must still resolve so is_w_tag matches
        // via the URI branch.
        let xml = r#"<document xmlns="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><body><p/></body></document>"#;
        let root = parse_document_xml_quick(xml.as_bytes()).unwrap();
        assert_eq!(root.prefix, None);
        assert_eq!(root.namespace.as_deref(), Some(WORD_NS));
        assert!(is_w_tag(&root, "document"));
        let body = find_w_child(&root, "body").unwrap();
        assert!(is_w_tag(body, "body"));
        assert_roundtrip_matches(xml);
    }

    #[test]
    fn quick_builder_namespaces_only_on_declaring_element() {
        // The whole point of Approach A: inner elements carry NO inherited
        // namespace map. Only elements that literally declare xmlns get one.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>"#;
        let root = parse_document_xml_quick(xml.as_bytes()).unwrap();
        assert!(root.namespaces.is_some(), "root declares xmlns:w");
        let body = find_w_child(&root, "body").unwrap();
        assert!(
            body.namespaces.is_none(),
            "child must not carry inherited namespace scope"
        );
        let p = find_w_child(body, "p").unwrap();
        assert!(p.namespaces.is_none());
    }

    #[test]
    fn quick_builder_rejects_doctype() {
        let xml = r#"<!DOCTYPE w:document [ <!ENTITY x "y"> ]><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>"#;
        match parse_document_xml_quick(xml.as_bytes()) {
            Err(WordXmlError::DoctypeRejected) => {}
            other => panic!("expected DoctypeRejected, got {other:?}"),
        }
    }

    #[test]
    fn quick_builder_matches_xml_rs_nested_namespace_decl() {
        // A child declares its own namespace (common in drawings). Both paths
        // must attach it to that child only and serialize identically.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:drawing><a:blip xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" r:embed="rId1" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/></w:drawing></w:r></w:p></w:body></w:document>"#;
        assert_roundtrip_matches(xml);
    }

    #[test]
    fn quick_builder_matches_xml_rs_cdata() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t><![CDATA[raw < & > text]]></w:t></w:r></w:p></w:body></w:document>"#;
        assert_roundtrip_matches(xml);
    }
}
