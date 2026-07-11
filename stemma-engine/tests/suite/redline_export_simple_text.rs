use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read};

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedChange {
    text: String,
    author: Option<String>,
    date: Option<String>,
}

#[test]
fn export_redline_for_samples_simple_text_matches_expected_payload() {
    let before_path = std::path::PathBuf::from("testdata").join("simple-text/before.docx");
    let after_path = std::path::PathBuf::from("testdata").join("simple-text/after.docx");
    let before_bytes = fs::read(&before_path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", before_path.display());
    });
    let after_bytes = fs::read(&after_path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", after_path.display());
    });

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let redline_meta = TransactionMeta {
        author: "redline_export_simple_text".to_string(),
        reason: Some("simple-text sample export contract".to_string()),
        timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
    };

    let apply = runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            redline_meta,
        )
        .expect("diff_and_redline should succeed");
    assert!(
        apply.applied,
        "redline apply result must be marked as applied"
    );

    let redline_docx = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline docx");
    assert!(
        !redline_docx.is_empty(),
        "exported redline DOCX must not be empty"
    );

    // Export contract: the generated package must be importable again.
    let verify_runtime = SimpleRuntime::new();
    verify_runtime
        .import_docx(&redline_docx)
        .expect("re-importing exported redline DOCX should succeed");

    let document_xml = extract_document_xml(&redline_docx);
    assert!(
        document_xml.contains("This is a test"),
        "unchanged leading text should remain in document.xml"
    );

    let parsed =
        Element::parse(Cursor::new(document_xml.as_bytes())).expect("parse word/document.xml");
    let (deleted, inserted) = collect_tracked_changes(&parsed);

    assert_eq!(
        deleted.len(),
        1,
        "expected exactly one deletion span in simple-text redline, got {deleted:?}"
    );
    assert_eq!(
        inserted.len(),
        1,
        "expected exactly one insertion span in simple-text redline, got {inserted:?}"
    );

    assert_eq!(normalize_ws(&deleted[0].text), "now foo bar baz");
    assert_eq!(normalize_ws(&inserted[0].text), "what are the chances");

    // Author must match the explicit value passed in TransactionMeta — there
    // is no default.
    assert_eq!(
        deleted[0].author.as_deref(),
        Some("redline_export_simple_text")
    );
    assert_eq!(
        inserted[0].author.as_deref(),
        Some("redline_export_simple_text")
    );

    assert_eq!(deleted[0].date.as_deref(), Some("2024-01-15T10:30:00Z"));
    assert_eq!(inserted[0].date.as_deref(), Some("2024-01-15T10:30:00Z"));

    // OOXML §17.13.5.18: w:id must be unique across all tracked change elements.
    let mut w_ids: Vec<String> = Vec::new();
    collect_w_ids(&parsed, &mut w_ids);
    assert!(
        !w_ids.is_empty(),
        "expected at least one w:id on tracked change elements"
    );
    let unique: HashSet<&str> = w_ids.iter().map(|s| s.as_str()).collect();
    assert_eq!(
        w_ids.len(),
        unique.len(),
        "w:id values must be unique across tracked change elements, but found duplicates: {w_ids:?}"
    );
}

fn extract_document_xml(docx_bytes: &[u8]) -> String {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open DOCX zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("DOCX must contain word/document.xml");

    let mut out = String::new();
    file.read_to_string(&mut out)
        .expect("read word/document.xml");
    out
}

fn collect_tracked_changes(root: &Element) -> (Vec<TrackedChange>, Vec<TrackedChange>) {
    let mut deleted = Vec::new();
    let mut inserted = Vec::new();
    collect_tracked_changes_recursive(root, &mut deleted, &mut inserted);
    (deleted, inserted)
}

fn collect_tracked_changes_recursive(
    element: &Element,
    deleted: &mut Vec<TrackedChange>,
    inserted: &mut Vec<TrackedChange>,
) {
    if is_w_tag(element, "del") {
        deleted.push(TrackedChange {
            text: collect_text(element),
            author: attr_value(element, "w:author")
                .or_else(|| attr_value(element, "author"))
                .map(ToString::to_string),
            date: attr_value(element, "w:date")
                .or_else(|| attr_value(element, "date"))
                .map(ToString::to_string),
        });
        return;
    }

    if is_w_tag(element, "ins") {
        inserted.push(TrackedChange {
            text: collect_text(element),
            author: attr_value(element, "w:author")
                .or_else(|| attr_value(element, "author"))
                .map(ToString::to_string),
            date: attr_value(element, "w:date")
                .or_else(|| attr_value(element, "date"))
                .map(ToString::to_string),
        });
        return;
    }

    for child in &element.children {
        if let XMLNode::Element(child_el) = child {
            collect_tracked_changes_recursive(child_el, deleted, inserted);
        }
    }
}

fn collect_text(element: &Element) -> String {
    let mut out = String::new();
    collect_text_recursive(element, &mut out);
    out
}

fn collect_text_recursive(element: &Element, out: &mut String) {
    for child in &element.children {
        match child {
            XMLNode::Text(text) => out.push_str(text),
            XMLNode::Element(child_el) => collect_text_recursive(child_el, out),
            _ => {}
        }
    }
}

fn is_w_tag(element: &Element, local: &str) -> bool {
    if local_name(&element.name) != local {
        return false;
    }

    if element.prefix.as_deref() == Some("w") {
        return true;
    }

    if element.namespace.as_deref() == Some(WORD_NS) {
        return true;
    }

    element.name == local || element.name == format!("w:{local}")
}

fn local_name(name: &str) -> &str {
    match name.rsplit_once(':') {
        Some((_, local)) => local,
        None => name,
    }
}

fn attr_value<'a>(element: &'a Element, qname: &str) -> Option<&'a str> {
    let (prefix, local) = split_qname(qname);

    if let Some(want_prefix) = prefix {
        for (name, value) in &element.attributes {
            if name.local_name == local && name.prefix.as_deref() == Some(want_prefix) {
                return Some(value);
            }
        }
    }

    for (name, value) in &element.attributes {
        if name.local_name == local {
            return Some(value);
        }
    }

    // Compatibility with legacy keys that were stored as plain strings.
    for (name, value) in &element.attributes {
        if name.local_name == qname {
            return Some(value);
        }
    }

    None
}

fn split_qname(qname: &str) -> (Option<&str>, &str) {
    match qname.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => (Some(prefix), local),
        _ => (None, qname),
    }
}

fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const TRACKED_CHANGE_TAGS: &[&str] = &["ins", "del", "cellIns", "cellDel", "rPrChange"];

fn collect_w_ids(element: &Element, ids: &mut Vec<String>) {
    let local = local_name(&element.name);
    if TRACKED_CHANGE_TAGS.contains(&local)
        && let Some(id) = attr_value(element, "w:id").or_else(|| attr_value(element, "id"))
    {
        ids.push(id.to_string());
    }
    for child in &element.children {
        if let XMLNode::Element(child_el) = child {
            collect_w_ids(child_el, ids);
        }
    }
}
