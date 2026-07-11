use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

fn generate_redline_docx() -> Vec<u8> {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before).expect("import before");
    let import_after = runtime.import_docx(&after).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "Stemma".to_string(),
                reason: Some("bookmark fidelity regression".to_string()),
                timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline");
    runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline")
}

fn read_document_xml(docx_bytes: &[u8]) -> String {
    let mut zip = ZipArchive::new(Cursor::new(docx_bytes)).expect("open zip");
    let mut file = zip.by_name("word/document.xml").expect("word/document.xml");
    let mut xml = String::new();
    file.read_to_string(&mut xml).expect("read document.xml");
    xml
}

fn read_document_rels_xml(docx_bytes: &[u8]) -> String {
    let mut zip = ZipArchive::new(Cursor::new(docx_bytes)).expect("open zip");
    let mut file = zip
        .by_name("word/_rels/document.xml.rels")
        .expect("word/_rels/document.xml.rels");
    let mut xml = String::new();
    file.read_to_string(&mut xml)
        .expect("read document.xml.rels");
    xml
}

/// Extract `attr="value"` from a single XML tag string.
fn tag_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let at = tag.find(&needle)? + needle.len();
    let rest = &tag[at..];
    Some(rest[..rest.find('"')?].to_string())
}

/// All `<w:bookmarkStart …>`/`<w:bookmarkEnd …>` tags with their byte offsets.
fn bookmark_tags<'a>(xml: &'a str, open: &str) -> Vec<(usize, &'a str)> {
    let mut out = Vec::new();
    let mut idx = 0;
    while let Some(pos) = xml[idx..].find(open) {
        let start = idx + pos;
        let end = start + xml[start..].find('>').expect("tag close") + 1;
        out.push((start, &xml[start..end]));
        idx = end;
    }
    out
}

#[test]
fn safe_singapore_redline_preserves_delta_view_bookmark_range() {
    let redline = generate_redline_docx();
    let xml = read_document_xml(&redline);

    // Match by ATTRIBUTES, not by byte order: the serializer now passes the
    // base bookmark through verbatim (ECMA-376 §17.13.6 — the id is the
    // pairing key and base ids are preserved), so the source's attribute
    // order (`w:id` before `w:name`) survives. The old needle baked in the
    // attribute order produced by the since-removed id remap.
    let starts: Vec<(usize, &str)> = bookmark_tags(&xml, "<w:bookmarkStart")
        .into_iter()
        .filter(|(_, tag)| tag_attr(tag, "w:name").as_deref() == Some("_DV_C50"))
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "redline should keep exactly one _DV_C50 bookmarkStart"
    );
    let (start_idx, start_tag) = starts[0];
    let start_id = tag_attr(start_tag, "w:id").expect("bookmarkStart w:id");

    // Exactly one end, and it pairs with the start by id (no torn pair).
    let ends = bookmark_tags(&xml, "<w:bookmarkEnd");
    assert_eq!(
        ends.len(),
        1,
        "redline should keep exactly one _DV_C50 bookmarkEnd"
    );
    let (end_idx, end_tag) = ends[0];
    assert_eq!(
        tag_attr(end_tag, "w:id").as_deref(),
        Some(start_id.as_str()),
        "bookmarkEnd must pair with the _DV_C50 bookmarkStart by id"
    );

    let cap_idx = start_idx
        + xml[start_idx..]
            .find("the Post-Money Valuation Cap")
            .expect("bookmarked start text");
    let company_idx = cap_idx
        + xml[cap_idx..]
            .find("by the Company Capitalization")
            .expect("bookmarked end text");

    assert!(
        start_idx < cap_idx,
        "bookmarkStart should stay before bookmarked deleted text"
    );
    assert!(
        company_idx < end_idx,
        "bookmarkEnd should stay after bookmarked deleted text"
    );
}

#[test]
fn safe_singapore_redline_document_relationship_ids_stay_type_consistent() {
    let redline = generate_redline_docx();
    let document_xml = read_document_xml(&redline);
    let rels_xml = read_document_rels_xml(&redline);

    let rels_root = Element::parse(Cursor::new(rels_xml)).expect("parse rels");
    let rels_by_id: HashMap<String, String> = rels_root
        .children
        .iter()
        .filter_map(|child| match child {
            XMLNode::Element(rel) => Some((
                rel.attributes
                    .iter()
                    .find_map(|(name, value)| (name.local_name == "Id").then(|| value.clone()))
                    .expect("Relationship@Id"),
                rel.attributes
                    .iter()
                    .find_map(|(name, value)| (name.local_name == "Type").then(|| value.clone()))
                    .expect("Relationship@Type"),
            )),
            _ => None,
        })
        .collect();

    let document_root = Element::parse(Cursor::new(document_xml)).expect("parse document");
    let mut hyperlink_ids = Vec::new();
    let mut header_footer_ids = Vec::new();
    collect_rel_ids(&document_root, &mut hyperlink_ids, &mut header_footer_ids);

    for rid in hyperlink_ids {
        let rel_type = rels_by_id
            .get(&rid)
            .unwrap_or_else(|| panic!("missing relationship for hyperlink {rid}"));
        assert!(
            rel_type.ends_with("/hyperlink"),
            "hyperlink r:id {rid} must resolve to hyperlink relationship, got {rel_type}"
        );
    }

    for (kind, rid) in header_footer_ids {
        let rel_type = rels_by_id
            .get(&rid)
            .unwrap_or_else(|| panic!("missing relationship for {kind} {rid}"));
        let expected_suffix = if kind == "headerReference" {
            "/header"
        } else {
            "/footer"
        };
        assert!(
            rel_type.ends_with(expected_suffix),
            "{kind} r:id {rid} must resolve to {expected_suffix}, got {rel_type}"
        );
    }
}

fn collect_rel_ids(
    element: &Element,
    hyperlink_ids: &mut Vec<String>,
    header_footer_ids: &mut Vec<(String, String)>,
) {
    let local_name = element.name.rsplit(':').next().expect("element local name");
    if let Some(rid) = element
        .attributes
        .iter()
        .find_map(|(name, value)| (name.local_name == "id").then(|| value.clone()))
    {
        if local_name == "hyperlink" {
            hyperlink_ids.push(rid);
        } else if local_name == "headerReference" || local_name == "footerReference" {
            header_footer_ids.push((local_name.to_string(), rid));
        }
    }

    for child in &element.children {
        if let XMLNode::Element(child_el) = child {
            collect_rel_ids(child_el, hyperlink_ids, header_footer_ids);
        }
    }
}
