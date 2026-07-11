use xmltree::{AttributeName, Element};

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const W14_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
pub(crate) const W16DU_NS: &str = "http://schemas.microsoft.com/office/word/2023/wordml/word16du";

pub(crate) fn attr_get<'a>(element: &'a Element, qname: &str) -> Option<&'a String> {
    let (want_prefix, want_local) = split_qname(qname);

    if let Some(prefix) = want_prefix {
        for (name, value) in &element.attributes {
            if name.local_name == want_local && name.prefix.as_deref() == Some(prefix) {
                return Some(value);
            }
        }
    }

    for (name, value) in &element.attributes {
        if name.local_name == want_local {
            return Some(value);
        }
    }

    // Fallback for legacy non-namespaced keys like "w:id" that may still exist.
    for (name, value) in &element.attributes {
        if name.local_name == qname {
            return Some(value);
        }
    }

    None
}

/// Capture every attribute of `element` whose local name is NOT in
/// `known_locals`, as `(qualified_name, value)` pairs — the attribute-level
/// "never silently drop" remainder for a MODELED element (RFC-0003). `xmlns`
/// declarations are skipped (they are namespace bindings, not data). Re-emit
/// each pair verbatim with `attr_set(el, &qname, &value)`.
pub(crate) fn capture_extra_attrs(
    element: &Element,
    known_locals: &[&str],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in &element.attributes {
        if name.prefix.as_deref() == Some("xmlns") || name.local_name == "xmlns" {
            continue;
        }
        if known_locals.contains(&name.local_name.as_str()) {
            continue;
        }
        let qname = match &name.prefix {
            Some(p) => format!("{p}:{}", name.local_name),
            None => name.local_name.clone(),
        };
        out.push((qname, value.clone()));
    }
    out
}

pub(crate) fn attr_set(element: &mut Element, qname: &str, value: impl AsRef<str>) {
    let name = attr_name(qname);

    // Replace existing key variant for the same attribute to avoid duplicates.
    let mut to_remove = Vec::new();
    for existing in element.attributes.keys() {
        if existing.local_name == name.local_name && existing.prefix == name.prefix {
            to_remove.push(existing.clone());
        }
    }
    for key in to_remove {
        element.attributes.shift_remove(&key);
    }

    element.attributes.insert(name, value.as_ref().to_string());
}

pub(crate) fn attr_name(qname: &str) -> AttributeName {
    let (prefix, local) = split_qname(qname);
    match prefix {
        Some(prefix) => match namespace_for_prefix(prefix) {
            Some(ns) => AttributeName::qualified(local, ns, Some(prefix)),
            None => AttributeName {
                local_name: local.to_string(),
                namespace: None,
                prefix: Some(prefix.to_string()),
            },
        },
        None => AttributeName::local(local),
    }
}

fn split_qname(qname: &str) -> (Option<&str>, &str) {
    match qname.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => (Some(prefix), local),
        _ => (None, qname),
    }
}

fn namespace_for_prefix(prefix: &str) -> Option<&'static str> {
    match prefix {
        "w" => Some(WORD_NS),
        "xml" => Some(XML_NS),
        "r" => Some(REL_NS),
        "mc" => Some(MC_NS),
        "w14" => Some(W14_NS),
        "w16du" => Some(W16DU_NS),
        _ => None,
    }
}
