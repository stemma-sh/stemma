//! Table redline audit tests: verify DOCX output matches Word's tracked-change
//! behavior for table row insertions, deletions, and cell content modifications.
//!
//! Each test imports a before/after fixture pair, generates a redline, exports
//! to DOCX, and then inspects the raw XML for correct tracked-change markup.
//!
//! These tests cover:
//! - Single table output (no duplication into 2 tables)
//! - Row-level tracking (w:ins/w:del in w:trPr)
//! - Inline tracked changes within cells
//! - w:delText for deleted content
//! - Accept/reject text reconstruction

use std::fs;
use std::io::{Cursor, Read};

use stemma::{
    DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta, redline_extract::extract_redline,
};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

// ── helpers ──────────────────────────────────────────────────────────────

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "table_redline_audit".to_string(),
        reason: Some("table redline audit test".to_string()),
        timestamp_utc: Some("2025-06-01T00:00:00Z".to_string()),
    }
}

/// Run the full redline pipeline on a before/after pair and return the
/// exported DOCX bytes.
fn run_redline_pipeline(before_path: &str, after_path: &str) -> Vec<u8> {
    let before_bytes =
        fs::read(before_path).unwrap_or_else(|err| panic!("read {before_path}: {err}"));
    let after_bytes = fs::read(after_path).unwrap_or_else(|err| panic!("read {after_path}: {err}"));

    let runtime = SimpleRuntime::new();

    let import_before = runtime
        .import_docx(&before_bytes)
        .unwrap_or_else(|err| panic!("import {before_path}: {err:?}"));
    let import_after = runtime
        .import_docx(&after_bytes)
        .unwrap_or_else(|err| panic!("import {after_path}: {err:?}"));

    let apply = runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            redline_meta(),
        )
        .unwrap_or_else(|err| panic!("diff_and_redline failed: {err:?}"));
    assert!(apply.applied, "redline must be marked as applied");

    let exported = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .unwrap_or_else(|err| panic!("export_docx failed: {err:?}"));
    assert!(!exported.is_empty(), "exported DOCX must not be empty");

    // Export contract: re-import must succeed.
    let verify = SimpleRuntime::new();
    verify
        .import_docx(&exported)
        .unwrap_or_else(|err| panic!("re-import of exported redline DOCX failed: {err:?}"));

    exported
}

/// Extract word/document.xml from DOCX bytes as a string.
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

fn parse_xml(xml_str: &str) -> Element {
    Element::parse(Cursor::new(xml_str.as_bytes())).expect("parse XML")
}

fn is_w_tag(element: &Element, local: &str) -> bool {
    let name = match element.name.rsplit_once(':') {
        Some((_, l)) => l,
        None => &element.name,
    };
    if name != local {
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

/// Find all elements with a given w: tag recursively.
fn find_all_w<'a>(root: &'a Element, tag: &str, out: &mut Vec<&'a Element>) {
    if is_w_tag(root, tag) {
        out.push(root);
    }
    for child in &root.children {
        if let XMLNode::Element(el) = child {
            find_all_w(el, tag, out);
        }
    }
}

/// Find a direct w: child element with a given tag.
fn find_w_child<'a>(parent: &'a Element, tag: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, tag) => Some(el),
        _ => None,
    })
}

/// Analysis of tracked-change structure in a redline DOCX.
struct RedlineAnalysis<'a> {
    root: &'a Element,
}

impl<'a> RedlineAnalysis<'a> {
    fn new(root: &'a Element) -> Self {
        Self { root }
    }

    /// Count w:tbl elements (top-level tables, not nested).
    fn table_count(&self) -> usize {
        let mut tbls = Vec::new();
        find_all_w(self.root, "tbl", &mut tbls);
        tbls.len()
    }

    /// Count rows that have w:ins inside w:trPr (inserted rows).
    fn rows_with_trpr_ins(&self) -> usize {
        let mut rows = Vec::new();
        find_all_w(self.root, "tr", &mut rows);
        rows.iter()
            .filter(|tr| {
                find_w_child(tr, "trPr").is_some_and(|trpr| find_w_child(trpr, "ins").is_some())
            })
            .count()
    }

    /// Count rows that have w:del inside w:trPr (deleted rows).
    fn rows_with_trpr_del(&self) -> usize {
        let mut rows = Vec::new();
        find_all_w(self.root, "tr", &mut rows);
        rows.iter()
            .filter(|tr| {
                find_w_child(tr, "trPr").is_some_and(|trpr| find_w_child(trpr, "del").is_some())
            })
            .count()
    }

    /// Check if any w:delText elements exist anywhere in the document.
    fn has_del_text(&self) -> bool {
        let mut found = Vec::new();
        find_all_w(self.root, "delText", &mut found);
        !found.is_empty()
    }

    /// Count inline w:ins elements (those NOT inside w:trPr — i.e., wrapping
    /// runs inside cells or paragraphs).
    fn inline_ins_count(&self) -> usize {
        count_inline_tracked_changes(self.root, "ins")
    }

    /// Count inline w:del elements (those NOT inside w:trPr).
    fn inline_del_count(&self) -> usize {
        count_inline_tracked_changes(self.root, "del")
    }

    /// Collect all text from w:delText elements.
    fn all_del_text_content(&self) -> String {
        let mut del_texts = Vec::new();
        find_all_w(self.root, "delText", &mut del_texts);
        let mut out = String::new();
        for dt in del_texts {
            for child in &dt.children {
                if let XMLNode::Text(t) = child {
                    out.push_str(t);
                }
            }
        }
        out
    }
}

/// Count w:ins or w:del elements that are NOT direct children of w:trPr.
/// This recursively walks the tree, skipping trPr children.
fn count_inline_tracked_changes(element: &Element, tag: &str) -> usize {
    let mut count = 0;
    count_inline_recursive(element, tag, false, &mut count);
    count
}

fn count_inline_recursive(element: &Element, tag: &str, inside_trpr: bool, count: &mut usize) {
    if is_w_tag(element, tag) && !inside_trpr {
        *count += 1;
    }
    let is_trpr = is_w_tag(element, "trPr");
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            count_inline_recursive(el, tag, is_trpr, count);
        }
    }
}

// ── long-table ───────────────────────────────────────────────────────────

const LONG_TABLE_BEFORE: &str = "testdata/long-table/before.docx";
const LONG_TABLE_AFTER: &str = "testdata/long-table/after.docx";

/// The long-table fixture should produce exactly 1 table (not 2).
/// Row "1a" should be marked as inserted, row "666" as deleted.
#[test]
fn long_table_single_table() {
    let exported = run_redline_pipeline(LONG_TABLE_BEFORE, LONG_TABLE_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert_eq!(
        analysis.table_count(),
        1,
        "long-table should produce 1 table, not {}",
        analysis.table_count()
    );
}

#[test]
fn long_table_row_level_tracking() {
    let exported = run_redline_pipeline(LONG_TABLE_BEFORE, LONG_TABLE_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert_eq!(
        analysis.rows_with_trpr_ins(),
        1,
        "long-table: exactly 1 row should have w:ins in trPr (the '1a' row), got {}",
        analysis.rows_with_trpr_ins()
    );
    assert_eq!(
        analysis.rows_with_trpr_del(),
        1,
        "long-table: exactly 1 row should have w:del in trPr (the '666' row), got {}",
        analysis.rows_with_trpr_del()
    );
}

#[test]
fn long_table_del_text() {
    let exported = run_redline_pipeline(LONG_TABLE_BEFORE, LONG_TABLE_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert!(
        analysis.has_del_text(),
        "long-table: should have w:delText elements for the '666' row content"
    );

    let del_content = analysis.all_del_text_content();
    assert!(
        del_content.contains("666"),
        "long-table: w:delText should contain '666', got: {del_content}"
    );
}

#[test]
fn long_table_accept_reject() {
    let exported = run_redline_pipeline(LONG_TABLE_BEFORE, LONG_TABLE_AFTER);
    let extract = extract_redline(&exported).expect("extract_redline");

    let accepted: String = extract
        .body
        .iter()
        .map(|p| p.accept_text())
        .collect::<Vec<_>>()
        .join("\n");
    let rejected: String = extract
        .body
        .iter()
        .map(|p| p.reject_text())
        .collect::<Vec<_>>()
        .join("\n");

    // Accept-all should contain inserted "1a" but not deleted "666"
    assert!(
        accepted.contains("1a"),
        "long-table accept-all should contain '1a'"
    );
    assert!(
        !accepted.contains("666"),
        "long-table accept-all should NOT contain '666'"
    );

    // Reject-all should contain deleted "666" but not inserted "1a"
    assert!(
        rejected.contains("666"),
        "long-table reject-all should contain '666'"
    );
    assert!(
        !rejected.contains("1a"),
        "long-table reject-all should NOT contain '1a'"
    );
}

// ── table-changes ────────────────────────────────────────────────────────

const TABLE_CHANGES_BEFORE: &str = "testdata/table-changes/before.docx";
const TABLE_CHANGES_AFTER: &str = "testdata/table-changes/after.docx";

/// table-changes: structure unchanged, only cell content modified.
/// Should produce 1 table with no row-level tracking.
#[test]
fn table_changes_single_table() {
    let exported = run_redline_pipeline(TABLE_CHANGES_BEFORE, TABLE_CHANGES_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert_eq!(
        analysis.table_count(),
        1,
        "table-changes should produce 1 table, not {}",
        analysis.table_count()
    );
}

#[test]
fn table_changes_no_row_level_tracking() {
    let exported = run_redline_pipeline(TABLE_CHANGES_BEFORE, TABLE_CHANGES_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert_eq!(
        analysis.rows_with_trpr_ins(),
        0,
        "table-changes: no rows should have w:ins in trPr (structure unchanged), got {}",
        analysis.rows_with_trpr_ins()
    );
    assert_eq!(
        analysis.rows_with_trpr_del(),
        0,
        "table-changes: no rows should have w:del in trPr (structure unchanged), got {}",
        analysis.rows_with_trpr_del()
    );
}

#[test]
fn table_changes_inline_tracked_changes() {
    let exported = run_redline_pipeline(TABLE_CHANGES_BEFORE, TABLE_CHANGES_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert!(
        analysis.inline_del_count() > 0,
        "table-changes: should have at least 1 inline w:del (cell content changed)"
    );
    assert!(
        analysis.has_del_text(),
        "table-changes: should have w:delText elements for deleted cell content"
    );
}

#[test]
fn table_changes_del_text_content() {
    let exported = run_redline_pipeline(TABLE_CHANGES_BEFORE, TABLE_CHANGES_AFTER);

    let extract = extract_redline(&exported).expect("extract_redline");
    let all_del = extract.all_deleted_text().join(" | ");

    assert!(
        all_del.contains("This is a"),
        "table-changes: deleted text should contain 'This is a', got: {all_del}"
    );
}

// ── table-modifications ──────────────────────────────────────────────────

const TABLE_MODS_BEFORE: &str = "testdata/table-modifications/before.docx";
const TABLE_MODS_AFTER: &str = "testdata/table-modifications/after.docx";

/// table-modifications: middle row deleted.
#[test]
fn table_modifications_single_table() {
    let exported = run_redline_pipeline(TABLE_MODS_BEFORE, TABLE_MODS_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert_eq!(
        analysis.table_count(),
        1,
        "table-modifications should produce 1 table, not {}",
        analysis.table_count()
    );
}

#[test]
fn table_modifications_row_level_tracking() {
    let exported = run_redline_pipeline(TABLE_MODS_BEFORE, TABLE_MODS_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert_eq!(
        analysis.rows_with_trpr_del(),
        1,
        "table-modifications: exactly 1 row should have w:del in trPr (the middle row), got {}",
        analysis.rows_with_trpr_del()
    );
    assert_eq!(
        analysis.rows_with_trpr_ins(),
        0,
        "table-modifications: no rows should have w:ins in trPr, got {}",
        analysis.rows_with_trpr_ins()
    );
}

#[test]
fn table_modifications_del_text() {
    let exported = run_redline_pipeline(TABLE_MODS_BEFORE, TABLE_MODS_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert!(
        analysis.has_del_text(),
        "table-modifications: should have w:delText for deleted row content"
    );
}

#[test]
fn table_modifications_accept_reject() {
    let exported = run_redline_pipeline(TABLE_MODS_BEFORE, TABLE_MODS_AFTER);
    let extract = extract_redline(&exported).expect("extract_redline");

    let all_del = extract.all_deleted_text().join(" | ");

    // The deleted row's content should appear as deleted text
    assert!(
        !all_del.is_empty(),
        "table-modifications: should have deleted text for the removed row"
    );

    // Reject-all should contain the deleted row content (restoring it).
    let rejected: String = extract
        .body
        .iter()
        .map(|p| p.reject_text())
        .collect::<Vec<_>>()
        .join("\n");

    // The deleted text must appear in the reject-all output.
    // We look for any deleted text fragment longer than 1 char to avoid
    // false matches on single digits/chars that appear elsewhere.
    let has_substantial_del = extract.all_deleted_text().iter().any(|t| {
        let trimmed = t.trim();
        trimmed.len() > 1 && trimmed != "\u{FFFC}"
    });
    if has_substantial_del {
        let substantial: Vec<_> = extract
            .all_deleted_text()
            .into_iter()
            .filter(|t| {
                let trimmed = t.trim();
                trimmed.len() > 1 && trimmed != "\u{FFFC}"
            })
            .collect();
        for del_text in &substantial {
            assert!(
                rejected.contains(del_text.trim()),
                "table-modifications: reject-all should contain deleted text '{}', got: {rejected}",
                del_text.trim()
            );
        }
    }
}

// ── table-row-deletion ───────────────────────────────────────────────────

const TABLE_ROW_DEL_BEFORE: &str = "testdata/table-row-deletion/before.docx";
const TABLE_ROW_DEL_AFTER: &str = "testdata/table-row-deletion/after.docx";

/// table-row-deletion: 1 row deleted, inline insertion for "Second " text.
#[test]
fn table_row_deletion_single_table() {
    let exported = run_redline_pipeline(TABLE_ROW_DEL_BEFORE, TABLE_ROW_DEL_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert_eq!(
        analysis.table_count(),
        1,
        "table-row-deletion should produce 1 table, not {}",
        analysis.table_count()
    );
}

#[test]
fn table_row_deletion_row_level_tracking() {
    let exported = run_redline_pipeline(TABLE_ROW_DEL_BEFORE, TABLE_ROW_DEL_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert_eq!(
        analysis.rows_with_trpr_del(),
        1,
        "table-row-deletion: exactly 1 row should have w:del in trPr, got {}",
        analysis.rows_with_trpr_del()
    );
}

#[test]
fn table_row_deletion_inline_insertion() {
    let exported = run_redline_pipeline(TABLE_ROW_DEL_BEFORE, TABLE_ROW_DEL_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    let analysis = RedlineAnalysis::new(&root);

    assert!(
        analysis.inline_ins_count() > 0,
        "table-row-deletion: should have at least 1 inline w:ins for 'Second ' text insertion"
    );
}

#[test]
fn table_row_deletion_accept_all() {
    let exported = run_redline_pipeline(TABLE_ROW_DEL_BEFORE, TABLE_ROW_DEL_AFTER);
    let extract = extract_redline(&exported).expect("extract_redline");

    let accepted: String = extract
        .body
        .iter()
        .map(|p| p.accept_text())
        .collect::<Vec<_>>()
        .join("\n");

    let rejected: String = extract
        .body
        .iter()
        .map(|p| p.reject_text())
        .collect::<Vec<_>>()
        .join("\n");

    // Accept-all should contain inserted text "Second"
    assert!(
        accepted.contains("Second"),
        "table-row-deletion: accept-all should contain inserted text 'Second'"
    );
    // Reject-all should NOT contain inserted text "Second"
    assert!(
        !rejected.contains("Second"),
        "table-row-deletion: reject-all should not contain inserted text 'Second'"
    );
    // Accept-all row count should differ from reject-all (one row deleted, text inserted)
    assert_ne!(
        accepted, rejected,
        "table-row-deletion: accept-all and reject-all should differ"
    );
}
