use std::fs;
use std::io::{Cursor, Read};

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;
const CUSTOM_XML_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml";
const CUSTOM_PROPERTIES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties";

fn generate_redline_docx(fixture: &str) -> Vec<u8> {
    let before_path = format!("testdata/{fixture}/before.docx");
    let after_path = format!("testdata/{fixture}/after.docx");
    let before = fs::read(&before_path).unwrap_or_else(|e| panic!("read {before_path}: {e}"));
    let after = fs::read(&after_path).unwrap_or_else(|e| panic!("read {after_path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before).expect("import before");
    let import_after = runtime.import_docx(&after).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "Stemma".to_string(),
                reason: Some("custom xml fidelity regression".to_string()),
                timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline");
    runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline")
}

fn read_part_from_docx(docx_bytes: &[u8], part_name: &str) -> Vec<u8> {
    let mut zip = ZipArchive::new(Cursor::new(docx_bytes)).expect("open zip");
    let mut file = zip
        .by_name(part_name)
        .unwrap_or_else(|e| panic!("{part_name}: {e}"));
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .unwrap_or_else(|e| panic!("read {part_name}: {e}"));
    bytes
}

fn read_part_from_fixture(fixture: &str, doc_name: &str, part_name: &str) -> Vec<u8> {
    let path = format!("testdata/{fixture}/{doc_name}.docx");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    read_part_from_docx(&bytes, part_name)
}

fn custom_xml_targets_from_docx(docx_bytes: &[u8]) -> Vec<String> {
    let rels = read_part_from_docx(docx_bytes, "word/_rels/document.xml.rels");
    let root = Element::parse(Cursor::new(rels)).expect("parse document rels");
    let mut targets = Vec::new();
    for child in &root.children {
        let XMLNode::Element(rel) = child else {
            continue;
        };
        if rel.name != "Relationship" && rel.name != "pr:Relationship" {
            continue;
        }
        let rel_type = rel
            .attributes
            .iter()
            .find_map(|(name, value)| {
                if name.local_name == "Type" {
                    Some(value.as_str())
                } else {
                    None
                }
            })
            .expect("Relationship@Type");
        if rel_type == CUSTOM_XML_REL_TYPE {
            targets.push(
                rel.attributes
                    .iter()
                    .find_map(|(name, value)| {
                        if name.local_name == "Target" {
                            Some(value.clone())
                        } else {
                            None
                        }
                    })
                    .expect("Relationship@Target"),
            );
        }
    }
    targets.sort();
    targets
}

fn root_relationship_targets_from_docx(docx_bytes: &[u8], rel_type: &str) -> Vec<String> {
    let rels = read_part_from_docx(docx_bytes, "_rels/.rels");
    let root = Element::parse(Cursor::new(rels)).expect("parse root rels");
    let mut targets = Vec::new();
    for child in &root.children {
        let XMLNode::Element(rel) = child else {
            continue;
        };
        if rel.name != "Relationship" && rel.name != "pr:Relationship" {
            continue;
        }
        let current_type = rel
            .attributes
            .iter()
            .find_map(|(name, value)| {
                if name.local_name == "Type" {
                    Some(value.as_str())
                } else {
                    None
                }
            })
            .expect("Relationship@Type");
        if current_type == rel_type {
            targets.push(
                rel.attributes
                    .iter()
                    .find_map(|(name, value)| {
                        if name.local_name == "Target" {
                            Some(value.clone())
                        } else {
                            None
                        }
                    })
                    .expect("Relationship@Target"),
            );
        }
    }
    targets.sort();
    targets
}

#[test]
fn safe_singapore_redline_preserves_target_custom_xml_relationships_and_payload() {
    let redline = generate_redline_docx("safe-us-vs-singapore");
    let expected_targets = custom_xml_targets_from_docx(
        &fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after"),
    );
    let actual_targets = custom_xml_targets_from_docx(&redline);

    assert_eq!(
        actual_targets, expected_targets,
        "redline should preserve target customXml document relationships"
    );

    let expected_item_props =
        read_part_from_fixture("safe-us-vs-singapore", "after", "customXml/itemProps1.xml");
    let actual_item_props = read_part_from_docx(&redline, "customXml/itemProps1.xml");
    assert_eq!(
        actual_item_props, expected_item_props,
        "redline should prefer target customXml payload for overlapping itemProps parts"
    );
}

#[test]
fn safe_cayman_redline_preserves_target_custom_xml_relationships() {
    let redline = generate_redline_docx("safe-us-vs-cayman");
    let expected_targets = custom_xml_targets_from_docx(
        &fs::read("testdata/safe-us-vs-cayman/after.docx").expect("read after"),
    );
    let actual_targets = custom_xml_targets_from_docx(&redline);

    assert_eq!(
        actual_targets, expected_targets,
        "redline should preserve target customXml document relationships"
    );
}

#[test]
fn safe_cayman_redline_preserves_target_custom_properties_part_and_root_relationship() {
    let redline = generate_redline_docx("safe-us-vs-cayman");
    let target_bytes = fs::read("testdata/safe-us-vs-cayman/after.docx").expect("read after");

    let expected_targets =
        root_relationship_targets_from_docx(&target_bytes, CUSTOM_PROPERTIES_REL_TYPE);
    let actual_targets = root_relationship_targets_from_docx(&redline, CUSTOM_PROPERTIES_REL_TYPE);
    assert_eq!(
        actual_targets, expected_targets,
        "redline should preserve target custom-properties root relationship"
    );

    let expected_custom = read_part_from_docx(&target_bytes, "docProps/custom.xml");
    let actual_custom = read_part_from_docx(&redline, "docProps/custom.xml");
    assert_eq!(
        actual_custom, expected_custom,
        "redline should preserve target custom properties payload"
    );
}
