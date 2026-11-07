//! Content controls bound to a data store (`w:dataBinding`, §17.5.2.6).
//!
//! A bound control shows the value of one node in an XML data store: the core
//! or extended document properties, or a custom XML part. Word fills the
//! control from the store whenever it opens the document, so an edit that
//! changes only the control's text is undone on open. Observed with Word on a
//! header control bound to the core title: a tracked edit of the control alone
//! vanished on open; with the title also changed, the tracked edit was kept and
//! Accept and Reject gave the new and old text. When Word itself edits a bound
//! control it writes the control's new text into the store.
//!
//! [`sync_bound_stores`] does the same after a transaction: for every binding
//! whose controls the transaction gave new text, it writes that text into the
//! bound node. Controls are read from the serialized story parts before and
//! after the transaction, so one rule covers every way the engine holds a
//! control (open block controls, sealed body controls, inline controls).

use std::collections::{BTreeMap, HashMap};

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, Writer};

use crate::docx_package::DocxPackage;
use crate::runtime::{ErrorCode, ErrorDetails, RuntimeError};

const WML: &[u8] = b"http://schemas.openxmlformats.org/wordprocessingml/2006/main";
/// Word's fixed store ids for the two built-in property stores.
const CORE_PROPERTIES_STORE: &str = "{6C3C8BC8-F283-45AE-878A-BAB7291924A1}";
const EXTENDED_PROPERTIES_STORE: &str = "{6668398D-A668-4E3E-A5EB-62B293D839F1}";
const CUSTOM_XML_PROPS_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXmlProps";

/// What a control is bound to. Controls sharing a binding show one value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Binding {
    /// `w:storeItemID`, upper-cased (GUIDs compare case-insensitively).
    store_item_id: String,
    xpath: String,
    prefix_mappings: String,
}

/// What one bound control shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reading {
    /// Its current text: tracked insertions included, deletions not.
    Text(String),
    /// Its placeholder (`w:showingPlcHdr`), which stands for no value.
    Placeholder,
    /// Nothing but deleted text: the control goes with its content on Accept.
    Removed,
}

/// Write into each bound store node the text a transaction gave its controls.
/// `before` and `after` are the packages on either side of the transaction;
/// `after` is updated. Returns whether any store part changed.
///
/// Refuses (no write is guessed) when controls sharing one binding were given
/// different texts, when the new text spans several paragraphs or lines (how
/// Word stores those in a node is not established), and when the binding's
/// XPath is beyond the plain absolute steps Word writes (`/p:name[n]/…`).
/// A binding whose store is not in the package has nothing to write.
pub(crate) fn sync_bound_stores(
    before: &DocxPackage,
    after: &mut DocxPackage,
) -> Result<bool, RuntimeError> {
    let old = bound_readings(before)?;
    let new = bound_readings(after)?;
    let mut changed = false;
    for (binding, readings) in &new {
        let previous = old.get(binding).map_or(&[][..], Vec::as_slice);
        let Some(value) = new_value(binding, previous, readings)? else {
            continue;
        };
        let Some(part) = store_part(after, &binding.store_item_id)? else {
            continue;
        };
        let bytes = after
            .get_part(&part)
            .expect("store_part names a part of this package");
        let steps = parse_xpath(binding)?;
        if let Some(rewritten) = write_node(bytes, &steps, &value)
            .map_err(|message| store_error(binding, &part, &message))?
        {
            after.set_part(&part, rewritten);
            changed = true;
        }
    }
    Ok(changed)
}

/// The text the transaction gave a binding's controls: a current reading no
/// earlier reading accounts for. None when it gave none.
fn new_value(
    binding: &Binding,
    previous: &[Reading],
    current: &[Reading],
) -> Result<Option<String>, RuntimeError> {
    let mut unmatched: Vec<&Reading> = previous.iter().collect();
    let mut produced: Vec<&str> = Vec::new();
    for reading in current {
        if let Some(at) = unmatched.iter().position(|p| *p == reading) {
            unmatched.remove(at);
        } else if let Reading::Text(text) = reading {
            produced.push(text);
        }
    }
    produced.sort_unstable();
    produced.dedup();
    match produced.as_slice() {
        [] => Ok(None),
        [value] if value.contains('\n') => Err(unsupported(format!(
            "the new text of the content control bound to {} in store {} spans several \
             paragraphs or lines, and how Word stores that in the bound node is not \
             established; edit it as one line",
            binding.xpath, binding.store_item_id
        ))),
        [value] => Ok(Some((*value).to_string())),
        several => Err(unsupported(format!(
            "content controls bound to {} in store {} show one value, but this edit gives \
             them different texts: {several:?}; give every control bound there the same text",
            binding.xpath, binding.store_item_id
        ))),
    }
}

/// Every bound control in the package's story parts, by binding, in part and
/// document order.
fn bound_readings(package: &DocxPackage) -> Result<BTreeMap<Binding, Vec<Reading>>, RuntimeError> {
    let main = package.main_document_part_name();
    let mut parts: Vec<&str> = package
        .part_names()
        .filter(|p| *p == main || is_story_part(p))
        .collect();
    parts.sort_unstable();
    let mut out: BTreeMap<Binding, Vec<Reading>> = BTreeMap::new();
    for part in parts {
        let bytes = package.get_part(part).expect("listed part");
        for (binding, reading) in read_controls(bytes)
            .map_err(|message| invalid(format!("{part}: content controls: {message}")))?
        {
            out.entry(binding).or_default().push(reading);
        }
    }
    Ok(out)
}

fn is_story_part(path: &str) -> bool {
    let Some(name) = path
        .strip_prefix("word/")
        .and_then(|p| p.strip_suffix(".xml"))
    else {
        return false;
    };
    let numbered = |stem: &str| {
        name.strip_prefix(stem)
            .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
    };
    matches!(name, "document" | "footnotes" | "endnotes")
        || numbered("header")
        || numbered("footer")
}

/// One open `w:sdt` while reading a story part.
#[derive(Default)]
struct Control {
    binding: Option<Binding>,
    placeholder: bool,
    in_content: bool,
    /// Finished paragraphs of the control's content.
    paragraphs: Vec<String>,
    /// The paragraph (or inline content) being read.
    current: String,
    deleted_text: bool,
}

impl Control {
    fn reading(mut self) -> Option<(Binding, Reading)> {
        let binding = self.binding?;
        if !self.current.is_empty() || self.paragraphs.is_empty() {
            self.paragraphs.push(std::mem::take(&mut self.current));
        }
        let text = self.paragraphs.join("\n");
        let reading = if self.placeholder {
            Reading::Placeholder
        } else if text.is_empty() && self.deleted_text {
            Reading::Removed
        } else {
            Reading::Text(text)
        };
        Some((binding, reading))
    }
}

/// The bound controls of one story part, in document order of their ends.
fn read_controls(bytes: &[u8]) -> Result<Vec<(Binding, Reading)>, String> {
    let mut reader = NsReader::from_reader(bytes);
    let mut buf = Vec::new();
    let mut open: Vec<Control> = Vec::new();
    let mut out = Vec::new();
    // Depth inside deleted or moved-away content, whose text is not shown.
    let mut deleted_depth = 0usize;
    let mut in_text = false;
    loop {
        let at = reader.buffer_position();
        let (namespace, event) = reader
            .read_resolved_event_into(&mut buf)
            .map_err(|e| format!("XML error after byte {at}: {e}"))?;
        let wml = matches!(namespace, ResolveResult::Bound(ns) if ns.as_ref() == WML);
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if wml => match e.local_name().as_ref() {
                b"sdt" if !empty => open.push(Control::default()),
                b"sdtContent" if !empty => {
                    if let Some(control) = open.last_mut() {
                        control.in_content = true;
                    }
                }
                b"dataBinding" => {
                    if let Some(control) = open.last_mut() {
                        control.binding = Some(binding_of(&e)?);
                    }
                }
                b"showingPlcHdr" => {
                    if let Some(control) = open.last_mut()
                        && !control.in_content
                    {
                        control.placeholder = true;
                    }
                }
                b"del" | b"moveFrom" if !empty => deleted_depth += 1,
                b"t" if !empty => in_text = true,
                b"delText" => {
                    for control in open.iter_mut().filter(|c| c.in_content) {
                        control.deleted_text = true;
                    }
                }
                b"tab" | b"br" | b"cr" if deleted_depth == 0 => {
                    let ch = if e.local_name().as_ref() == b"tab" {
                        '\t'
                    } else {
                        '\n'
                    };
                    for control in open.iter_mut().filter(|c| c.in_content) {
                        control.current.push(ch);
                    }
                }
                _ => {}
            },
            Event::End(e) if wml => match e.local_name().as_ref() {
                b"sdt" => {
                    let control = open.pop().ok_or("w:sdt end without a start")?;
                    out.extend(control.reading());
                }
                b"del" | b"moveFrom" => deleted_depth = deleted_depth.saturating_sub(1),
                b"t" => in_text = false,
                b"p" => {
                    for control in open.iter_mut().filter(|c| c.in_content) {
                        let paragraph = std::mem::take(&mut control.current);
                        control.paragraphs.push(paragraph);
                    }
                }
                _ => {}
            },
            Event::Text(text) if in_text => {
                let text = text.unescape().map_err(|e| e.to_string())?;
                for control in open.iter_mut().filter(|c| c.in_content) {
                    if deleted_depth == 0 {
                        control.current.push_str(&text);
                    } else {
                        control.deleted_text = true;
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn binding_of(element: &BytesStart) -> Result<Binding, String> {
    let mut store_item_id = None;
    let mut xpath = None;
    let mut prefix_mappings = String::new();
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|e| e.to_string())?;
        let value = attribute
            .unescape_value()
            .map_err(|e| e.to_string())?
            .into_owned();
        match attribute.key.local_name().as_ref() {
            b"storeItemID" => store_item_id = Some(value.to_ascii_uppercase()),
            b"xpath" => xpath = Some(value),
            b"prefixMappings" => prefix_mappings = value,
            _ => {}
        }
    }
    Ok(Binding {
        store_item_id: store_item_id.ok_or("w:dataBinding without w:storeItemID")?,
        xpath: xpath.ok_or("w:dataBinding without w:xpath")?,
        prefix_mappings,
    })
}

/// The part holding a store: the property parts for Word's two built-in
/// stores, else the custom XML item whose properties carry the id.
fn store_part(package: &DocxPackage, store_item_id: &str) -> Result<Option<String>, RuntimeError> {
    let fixed = match store_item_id {
        CORE_PROPERTIES_STORE => Some("docProps/core.xml"),
        EXTENDED_PROPERTIES_STORE => Some("docProps/app.xml"),
        _ => None,
    };
    if let Some(path) = fixed {
        return Ok(package.get_part(path).is_some().then(|| path.to_string()));
    }
    // customXml/itemN.xml is described by the itemProps part its own
    // relationships name.
    let items: Vec<String> = package
        .part_names()
        .filter(|p| {
            p.strip_prefix("customXml/item")
                .and_then(|n| n.strip_suffix(".xml"))
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_string)
        .collect();
    let mut found = None;
    for item in items {
        let rels_path = item.replacen("customXml/", "customXml/_rels/", 1) + ".rels";
        // OPC part names compare case-insensitively.
        let Some(rels) = package
            .story_rels
            .iter()
            .find(|(path, _)| path.eq_ignore_ascii_case(&rels_path))
            .map(|(_, rels)| rels)
        else {
            continue;
        };
        for relationship in rels
            .entries
            .iter()
            .filter(|r| r.rel_type == CUSTOM_XML_PROPS_REL)
        {
            let props_path = rels.resolve_internal_target(&relationship.target);
            let Some(props) = package.get_part(&props_path) else {
                return Err(invalid(format!(
                    "{rels_path} names {props_path}, which is not in the package"
                )));
            };
            let id = item_id(props).map_err(|m| invalid(format!("{props_path}: {m}")))?;
            if id.map(|id| id.to_ascii_uppercase()).as_deref() == Some(store_item_id) {
                if found.is_some() {
                    return Err(invalid(format!(
                        "two custom XML parts carry the store id {store_item_id}"
                    )));
                }
                found = Some(item.clone());
            }
        }
    }
    Ok(found)
}

fn item_id(bytes: &[u8]) -> Result<Option<String>, String> {
    let mut reader = NsReader::from_reader(bytes);
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| e.to_string())?
        {
            Event::Eof => return Ok(None),
            Event::Start(e) | Event::Empty(e) => {
                for attribute in e.attributes() {
                    let attribute = attribute.map_err(|e| e.to_string())?;
                    if attribute.key.local_name().as_ref() == b"itemID" {
                        return Ok(Some(
                            attribute
                                .unescape_value()
                                .map_err(|e| e.to_string())?
                                .into_owned(),
                        ));
                    }
                }
                return Ok(None);
            }
            _ => {}
        }
        buf.clear();
    }
}

/// One step of a bound XPath: an element by namespace, local name and
/// 1-based position among its same-named siblings.
#[derive(Debug, PartialEq, Eq)]
struct Step {
    namespace: Option<String>,
    local: String,
    position: usize,
}

/// Parse the plain absolute paths Word writes (`/ns0:a[1]/ns1:b[1]`). Any
/// other XPath is refused rather than approximated.
fn parse_xpath(binding: &Binding) -> Result<Vec<Step>, RuntimeError> {
    let refuse = |why: &str| {
        unsupported(format!(
            "the content control bound to {} in store {} cannot have its store updated: {why}",
            binding.xpath, binding.store_item_id
        ))
    };
    let prefixes = parse_prefix_mappings(&binding.prefix_mappings)
        .ok_or_else(|| refuse("its w:prefixMappings cannot be read"))?;
    let path = binding
        .xpath
        .strip_prefix('/')
        .filter(|p| !p.is_empty() && !p.starts_with('/'))
        .ok_or_else(|| refuse("only absolute element paths are supported"))?;
    path.split('/')
        .map(|step| {
            let (name, position) = match step.split_once('[') {
                Some((name, rest)) => {
                    let position = rest
                        .strip_suffix(']')
                        .and_then(|n| n.parse::<usize>().ok())
                        .filter(|n| *n >= 1)
                        .ok_or_else(|| refuse("a step's predicate is not a position"))?;
                    (name, position)
                }
                None => (step, 1),
            };
            let (namespace, local) = match name.split_once(':') {
                Some((prefix, local)) => (
                    Some(
                        prefixes
                            .get(prefix)
                            .ok_or_else(|| refuse("a step uses an undeclared prefix"))?
                            .clone(),
                    ),
                    local,
                ),
                None => (None, name),
            };
            let is_name = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
            };
            if !is_name(local) {
                return Err(refuse("a step is not an element name"));
            }
            Ok(Step {
                namespace,
                local: local.to_string(),
                position,
            })
        })
        .collect()
}

/// `xmlns:ns0='uri' xmlns:ns1="uri"` → prefix → namespace.
pub(crate) fn parse_prefix_mappings(mappings: &str) -> Option<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut rest = mappings.trim();
    while !rest.is_empty() {
        let after = rest.strip_prefix("xmlns:")?;
        let (prefix, after) = after.split_once('=')?;
        let quote = after.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
        let (uri, after) = after[1..].split_once(quote)?;
        out.insert(prefix.trim().to_string(), uri.to_string());
        rest = after.trim_start();
    }
    Some(out)
}

/// An element's namespace (None when unqualified) and local name.
type ElementName = (Option<Vec<u8>>, Vec<u8>);

/// Set the text of the element `steps` addresses, keeping every other byte
/// of the part. The element is created as the last child of its parent when
/// absent (the store has no such value yet). Returns None when it already
/// holds `value`.
fn write_node(bytes: &[u8], steps: &[Step], value: &str) -> Result<Option<Vec<u8>>, String> {
    let mut reader = NsReader::from_reader(bytes);
    let mut writer = Writer::new(Vec::with_capacity(bytes.len() + value.len()));
    let mut buf = Vec::new();
    // Per open element: whether it is on the path, and its children's counts.
    let mut on_path: Vec<bool> = Vec::new();
    let mut counts: Vec<HashMap<ElementName, usize>> = vec![HashMap::new()];
    let mut old_text: Option<String> = None;
    let mut written = false;
    let last = steps.len() - 1;
    loop {
        let at = reader.buffer_position();
        let (namespace, event) = reader
            .read_resolved_event_into(&mut buf)
            .map_err(|e| format!("XML error after byte {at}: {e}"))?;
        let namespace = match namespace {
            ResolveResult::Bound(ns) => Some(ns.as_ref().to_vec()),
            ResolveResult::Unbound => None,
            ResolveResult::Unknown(prefix) => {
                return Err(format!(
                    "undeclared prefix {}",
                    String::from_utf8_lossy(&prefix)
                ));
            }
        };
        match &event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let depth = on_path.len();
                let key = (namespace.clone(), e.local_name().as_ref().to_vec());
                let count = {
                    let seen = counts.last_mut().expect("a sibling map per open element");
                    let n = seen.entry(key).or_insert(0);
                    *n += 1;
                    *n
                };
                let matches = depth < steps.len()
                    && (depth == 0 || on_path[depth - 1])
                    && namespace.as_deref() == steps[depth].namespace.as_deref().map(str::as_bytes)
                    && e.local_name().as_ref() == steps[depth].local.as_bytes()
                    && count == steps[depth].position;
                if matches && depth == last {
                    let start = e.to_owned();
                    let end =
                        BytesEnd::new(String::from_utf8_lossy(e.name().as_ref()).into_owned());
                    let text = if matches!(event, Event::Start(_)) {
                        read_leaf_text(&mut reader)?
                    } else {
                        String::new()
                    };
                    old_text = Some(text);
                    writer
                        .write_event(Event::Start(start))
                        .and_then(|()| writer.write_event(Event::Text(BytesText::new(value))))
                        .and_then(|()| writer.write_event(Event::End(end)))
                        .map_err(|e| e.to_string())?;
                    written = true;
                    buf.clear();
                    continue;
                }
                if matches!(event, Event::Start(_)) {
                    on_path.push(matches);
                    counts.push(HashMap::new());
                } else if matches && depth + 1 == last && !written {
                    // An empty parent of an absent target: open it to hold the target.
                    let end =
                        BytesEnd::new(String::from_utf8_lossy(e.name().as_ref()).into_owned());
                    writer
                        .write_event(Event::Start(e.to_owned()))
                        .map_err(|e| e.to_string())?;
                    write_new_node(&mut writer, &steps[last], &HashMap::new(), value)?;
                    writer
                        .write_event(Event::End(end))
                        .map_err(|e| e.to_string())?;
                    old_text = Some(String::new());
                    written = true;
                    buf.clear();
                    continue;
                }
            }
            Event::End(_) => {
                let depth = on_path.len() - 1;
                // The parent of an absent target: add the target before its end.
                if !written && depth + 1 == last && on_path[depth] {
                    let present = counts.last().expect("the parent's sibling map");
                    write_new_node(&mut writer, &steps[last], present, value)?;
                    old_text = Some(String::new());
                    written = true;
                }
                on_path.pop();
                counts.pop();
            }
            _ => {}
        }
        writer.write_event(event).map_err(|e| e.to_string())?;
        buf.clear();
    }
    match old_text {
        None => Err("the bound node and its parent are not in the store".to_string()),
        Some(old) if old == value && written => Ok(None),
        Some(_) => Ok(Some(writer.into_inner())),
    }
}

/// Write the absent target element (`step`) holding `value`, after the
/// parent's existing children counted in `present`. Only the next position
/// can be created.
fn write_new_node(
    writer: &mut Writer<Vec<u8>>,
    step: &Step,
    present: &HashMap<ElementName, usize>,
    value: &str,
) -> Result<(), String> {
    let key = (
        step.namespace.as_ref().map(|n| n.as_bytes().to_vec()),
        step.local.as_bytes().to_vec(),
    );
    let count = present.get(&key).copied().unwrap_or(0);
    if count + 1 != step.position {
        return Err(format!(
            "the store has {count} {} element(s) there, so position {} cannot be created",
            step.local, step.position
        ));
    }
    let mut start = BytesStart::new(step.local.clone());
    if let Some(ns) = &step.namespace {
        start.push_attribute(("xmlns", ns.as_str()));
    }
    writer
        .write_event(Event::Start(start))
        .and_then(|()| writer.write_event(Event::Text(BytesText::new(value))))
        .and_then(|()| writer.write_event(Event::End(BytesEnd::new(step.local.clone()))))
        .map_err(|e| e.to_string())
}

/// Read a bound element's text up to its end. A bound node holds text only.
fn read_leaf_text(reader: &mut NsReader<&[u8]>) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut text = String::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| e.to_string())?
        {
            Event::End(_) => return Ok(text),
            Event::Text(t) => text.push_str(&t.unescape().map_err(|e| e.to_string())?),
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::Comment(_) | Event::PI(_) => {}
            Event::Start(_) | Event::Empty(_) => {
                return Err("the bound node holds elements, not a value".to_string());
            }
            Event::Eof => return Err("the store ends inside the bound node".to_string()),
            Event::Decl(_) | Event::DocType(_) => {}
        }
        buf.clear();
    }
}

fn store_error(binding: &Binding, part: &str, message: &str) -> RuntimeError {
    unsupported(format!(
        "the content control bound to {} cannot have its store {part} updated: {message}",
        binding.xpath
    ))
}

fn unsupported(message: String) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::UnsupportedEdit,
        message,
        details: ErrorDetails {
            context: Some("content_control_data_binding".to_string()),
            ..ErrorDetails::default()
        },
    }
}

fn invalid(message: String) -> RuntimeError {
    crate::runtime::invalid_docx(&message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(xpath: &str, mappings: &str) -> Binding {
        Binding {
            store_item_id: CORE_PROPERTIES_STORE.to_string(),
            xpath: xpath.to_string(),
            prefix_mappings: mappings.to_string(),
        }
    }

    const CORE_MAPPINGS: &str = "xmlns:ns0='http://schemas.openxmlformats.org/package/2006/metadata/core-properties' xmlns:ns1='http://purl.org/dc/elements/1.1/'";

    #[test]
    fn a_word_xpath_parses_to_namespaced_steps() {
        let steps = parse_xpath(&binding(
            "/ns0:coreProperties[1]/ns1:title[1]",
            CORE_MAPPINGS,
        ))
        .unwrap();
        assert_eq!(
            steps[1].namespace.as_deref(),
            Some("http://purl.org/dc/elements/1.1/")
        );
        assert_eq!((steps[1].local.as_str(), steps[1].position), ("title", 1));
    }

    #[test]
    fn other_xpaths_are_refused() {
        for xpath in [
            "//ns1:title",
            "/ns0:coreProperties[1]/@x",
            "/ns0:a[last()]",
            "/nsX:a[1]",
        ] {
            assert!(
                parse_xpath(&binding(xpath, CORE_MAPPINGS)).is_err(),
                "{xpath}"
            );
        }
    }

    #[test]
    fn writing_a_node_keeps_every_other_byte() {
        let core = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Civil &amp; Co</dc:title><dc:creator>A</dc:creator></cp:coreProperties>"#;
        let steps = parse_xpath(&binding(
            "/ns0:coreProperties[1]/ns1:title[1]",
            CORE_MAPPINGS,
        ))
        .unwrap();
        let out = write_node(core, &steps, "Criminal & Co")
            .unwrap()
            .expect("changed");
        let expected = String::from_utf8_lossy(core).replace("Civil &amp; Co", "Criminal &amp; Co");
        assert_eq!(String::from_utf8(out).unwrap(), expected);
        assert_eq!(
            write_node(core, &steps, "Civil & Co").unwrap(),
            None,
            "same value, no write"
        );
    }

    #[test]
    fn an_absent_node_is_created_under_its_parent() {
        let core = br#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"><cp:category>x</cp:category></cp:coreProperties>"#;
        let steps = parse_xpath(&binding(
            "/ns0:coreProperties[1]/ns1:title[1]",
            CORE_MAPPINGS,
        ))
        .unwrap();
        let out = String::from_utf8(write_node(core, &steps, "T").unwrap().unwrap()).unwrap();
        assert!(
            out.ends_with(r#"<cp:category>x</cp:category><title xmlns="http://purl.org/dc/elements/1.1/">T</title></cp:coreProperties>"#),
            "{out}"
        );
    }

    #[test]
    fn a_control_reads_its_shown_text() {
        let part = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:sdt><w:sdtPr><w:dataBinding w:prefixMappings="" w:xpath="/a[1]" w:storeItemID="{abc}"/></w:sdtPr><w:sdtContent><w:p><w:del w:id="1"><w:r><w:delText>Civil</w:delText></w:r></w:del><w:ins w:id="2"><w:r><w:t>Criminal</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> Appeal</w:t></w:r></w:p></w:sdtContent></w:sdt></w:body></w:document>"#;
        let controls = read_controls(part).unwrap();
        assert_eq!(controls.len(), 1);
        assert_eq!(controls[0].0.store_item_id, "{ABC}");
        assert_eq!(controls[0].1, Reading::Text("Criminal Appeal".to_string()));
    }
}
