//! Regression tests for the SAFE US vs Cayman Islands document comparison.
//!
//! This test validates that paragraph alignment correctly handles inserted
//! paragraphs at the document start. The Cayman document adds a new first
//! paragraph that wasn't in the US version.
//!
//! ## Key regression test:
//!
//! The Cayman document inserts "Please seek advice from an attorney licensed
//! in the Cayman Islands..." as a NEW first paragraph. The alignment algorithm
//! must:
//! 1. Emit a BlockInserted for this new paragraph
//! 2. NOT misalign it with the Securities Act paragraph from doc1
//!
//! ## Bug this tests for:
//!
//! A bug in the DP backtracking logic caused the alignment to match:
//! - doc1_p1 (Securities Act) → doc2_p1 (Cayman advisory) with 2.4% similarity
//!
//! Instead of the correct alignment:
//! - Inserted: doc2_p1 (Cayman advisory)
//! - Modified: doc1_p1 (Securities Act) → doc2_p2 (Securities Act) with 89.7% similarity
//!
//! ## Full document view regression:
//!
//! The Securities Act paragraph is modified between US and Cayman versions:
//! - "securities laws of certain states" → "securities laws of any other jurisdiction"
//! - "pledged or hypothecated" → "subject to security or hypothecated"
//! - "under the act" → "under the securities act"
//!
//! The full document view must report this paragraph as "modified" (not "unchanged")
//! with inline diff segments showing these changes.

use std::fs;
use std::io::{Cursor, Read};
use std::sync::LazyLock;

use stemma::{
    BlockNode, ChangeType, DiffChange, DocxRuntime, ExportMode, FullDocBlock, InlineChange,
    InlineNode, SimpleRuntime, TransactionMeta,
};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

// =============================================================================
// Test helpers
// =============================================================================

fn extract_inline_text(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => out.push_str(&t.text),
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(_) => out.push('\u{FFFC}'),
            InlineNode::Decoration(_) => {}
            InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
        }
    }
    out
}

fn block_text(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(p) => {
            let inlines = p.all_inlines_owned();
            extract_inline_text(&inlines)
        }
        _ => String::new(),
    }
}

fn generate_redline_docx() -> Vec<u8> {
    let before_bytes =
        fs::read("testdata/safe-us-vs-cayman/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-cayman/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "Stemma".to_string(),
                reason: Some("safe cayman structural marker fidelity regression".to_string()),
                timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline");

    runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline")
}

fn is_w_tag(element: &Element, local_name: &str) -> bool {
    element.name.rsplit(':').next() == Some(local_name)
}

fn tracked_container_descendants_with_structural_markers(
    docx_bytes: &[u8],
) -> Vec<(String, String, String)> {
    let mut zip = ZipArchive::new(Cursor::new(docx_bytes)).expect("open zip");
    let mut findings = Vec::new();

    let structural_marker_names = [
        "proofErr",
        "bookmarkStart",
        "bookmarkEnd",
        "commentRangeStart",
        "commentRangeEnd",
        "commentReference",
        "permStart",
        "permEnd",
        "moveFromRangeStart",
        "moveFromRangeEnd",
        "moveToRangeStart",
        "moveToRangeEnd",
        "customXmlInsRangeStart",
        "customXmlInsRangeEnd",
        "customXmlDelRangeStart",
        "customXmlDelRangeEnd",
        "customXmlMoveFromRangeStart",
        "customXmlMoveFromRangeEnd",
        "customXmlMoveToRangeStart",
        "customXmlMoveToRangeEnd",
    ];

    for index in 0..zip.len() {
        let mut file = zip.by_index(index).expect("zip entry");
        let name = file.name().to_string();
        if !name.starts_with("word/") || !name.ends_with(".xml") {
            continue;
        }

        let mut xml = String::new();
        file.read_to_string(&mut xml)
            .unwrap_or_else(|e| panic!("read {name}: {e}"));
        let root = Element::parse(Cursor::new(xml)).unwrap_or_else(|e| panic!("parse {name}: {e}"));

        collect_nested_structural_markers(&root, &name, &structural_marker_names, &mut findings);
    }

    findings
}

fn read_part_xml_from_docx(docx_bytes: &[u8], part_name: &str) -> String {
    let mut zip = ZipArchive::new(Cursor::new(docx_bytes)).expect("open zip");
    let mut file = zip
        .by_name(part_name)
        .unwrap_or_else(|e| panic!("{part_name}: {e}"));
    let mut xml = String::new();
    file.read_to_string(&mut xml)
        .unwrap_or_else(|e| panic!("read {part_name}: {e}"));
    xml
}

fn collect_nested_structural_markers(
    element: &Element,
    part_name: &str,
    structural_marker_names: &[&str],
    findings: &mut Vec<(String, String, String)>,
) {
    if let Some(container_name) = ["ins", "del", "moveFrom", "moveTo"]
        .into_iter()
        .find(|name| is_w_tag(element, name))
        && let Some(marker_name) = find_nested_structural_marker(element, structural_marker_names)
    {
        findings.push((
            part_name.to_string(),
            container_name.to_string(),
            marker_name.to_string(),
        ));
    }

    for child in &element.children {
        if let XMLNode::Element(child_el) = child {
            collect_nested_structural_markers(
                child_el,
                part_name,
                structural_marker_names,
                findings,
            );
        }
    }
}

fn find_nested_structural_marker<'a>(
    container: &'a Element,
    structural_marker_names: &[&str],
) -> Option<&'a str> {
    for child in &container.children {
        let XMLNode::Element(child_el) = child else {
            continue;
        };
        if structural_marker_names
            .iter()
            .any(|name| is_w_tag(child_el, name))
        {
            return child_el.name.rsplit(':').next();
        }
        if let Some(found) = find_nested_structural_marker(child_el, structural_marker_names) {
            return Some(found);
        }
    }
    None
}

fn deleted_field_shell_descendants(element: &Element, findings: &mut Vec<String>) {
    if is_w_tag(element, "del")
        && let Some(offender) =
            find_descendant_w_tag(element, &["fldChar", "instrText", "delInstrText"])
    {
        findings.push(offender.to_string());
    }

    for child in &element.children {
        if let XMLNode::Element(child_el) = child {
            deleted_field_shell_descendants(child_el, findings);
        }
    }
}

fn find_descendant_w_tag<'a>(element: &'a Element, local_names: &[&str]) -> Option<&'a str> {
    for child in &element.children {
        let XMLNode::Element(child_el) = child else {
            continue;
        };
        if local_names.iter().any(|name| is_w_tag(child_el, name)) {
            return child_el.name.rsplit(':').next();
        }
        if let Some(found) = find_descendant_w_tag(child_el, local_names) {
            return Some(found);
        }
    }
    None
}

/// Aggregated diff content for assertions.
struct DiffSummary {
    /// Text from modified blocks (old/before version)
    modified_old_texts: Vec<String>,
    /// Text from modified blocks (new/after version)
    modified_new_texts: Vec<String>,
    /// Text from deleted blocks
    deleted_texts: Vec<String>,
    /// Text from inserted blocks
    inserted_texts: Vec<String>,
}

impl DiffSummary {
    /// Check if any inserted block contains the text.
    fn inserted_contains(&self, needle: &str) -> bool {
        self.inserted_texts.iter().any(|t| t.contains(needle))
    }

    /// Check if any modified (new) block contains the text.
    fn modified_new_contains(&self, needle: &str) -> bool {
        self.modified_new_texts.iter().any(|t| t.contains(needle))
    }

    /// Check if any modified (old) block contains the text.
    fn modified_old_contains(&self, needle: &str) -> bool {
        self.modified_old_texts.iter().any(|t| t.contains(needle))
    }
}

fn summarize_diff(changes: &[DiffChange]) -> DiffSummary {
    let mut summary = DiffSummary {
        modified_old_texts: Vec::new(),
        modified_new_texts: Vec::new(),
        deleted_texts: Vec::new(),
        inserted_texts: Vec::new(),
    };

    for change in changes {
        match change {
            DiffChange::BlockModified {
                old_text, new_text, ..
            } => {
                summary.modified_old_texts.push(old_text.clone());
                summary.modified_new_texts.push(new_text.clone());
            }
            DiffChange::BlockDeleted { old_text, .. } => {
                summary.deleted_texts.push(old_text.clone());
            }
            DiffChange::BlockInserted { block, .. } => {
                summary.inserted_texts.push(block_text(block));
            }
            _ => {}
        }
    }

    summary
}

/// Cached diff result shared across all tests that only need the diff summary.
static CACHED_DIFF: LazyLock<(DiffSummary, stemma::DocumentDiff)> = LazyLock::new(|| {
    let before_bytes =
        fs::read("testdata/safe-us-vs-cayman/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-cayman/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let diff = runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff should succeed");

    let summary = summarize_diff(&diff.changes);
    (summary, diff)
});

// =============================================================================
// Regression tests for paragraph alignment at document start
// =============================================================================

/// CRITICAL REGRESSION TEST: The Cayman advisory must appear as an INSERTED block.
///
/// This tests the fix for a bug in the DP backtracking logic where gap-opening
/// operations (InsertOpen/DeleteOpen) incorrectly assumed they came from State::M
/// instead of checking which state (M, I, or D) was actually cheaper.
///
/// Without the fix, the alignment was:
/// - Modified: doc1_p1 (Securities Act) → doc2_p1 (Cayman advisory) [WRONG - 2.4% similarity]
///
/// With the fix, the alignment is:
/// - Inserted: doc2_p1 (Cayman advisory) [CORRECT]
/// - Modified: doc1_p1 (Securities Act) → doc2_p2 (Securities Act) [CORRECT - 89.7% similarity]
#[test]
fn cayman_advisory_is_inserted_not_modified() {
    let (summary, _) = &*CACHED_DIFF;

    // The Cayman advisory MUST appear as an inserted block
    // Use specific text to distinguish from other "Cayman Islands" mentions
    assert!(
        summary.inserted_contains("attorney licensed in the Cayman Islands"),
        "Cayman advisory should be INSERTED, not modified.\n\
         Inserted texts: {:?}\n\
         This is a regression - the alignment algorithm may be misaligning paragraphs.",
        summary.inserted_texts
    );
}

/// The Securities Act paragraph should appear in modified blocks (both old and new).
///
/// This verifies the alignment correctly matches the Securities Act paragraphs
/// between doc1 and doc2, rather than misaligning with the Cayman advisory.
#[test]
fn securities_act_paragraph_is_modified() {
    let (summary, _) = &*CACHED_DIFF;

    // The Securities Act text should appear in BOTH modified_old and modified_new
    assert!(
        summary.modified_old_contains("SECURITIES ACT"),
        "Securities Act should appear in modified old texts.\n\
         Modified old texts: {:?}",
        summary.modified_old_texts
    );

    assert!(
        summary.modified_new_contains("SECURITIES ACT"),
        "Securities Act should appear in modified new texts.\n\
         Modified new texts: {:?}",
        summary.modified_new_texts
    );
}

/// The Cayman advisory should NOT appear in modified blocks.
///
/// If it appears as modified, the alignment is wrong - it means the algorithm
/// paired it with an unrelated paragraph from doc1.
#[test]
fn cayman_advisory_not_in_modified_blocks() {
    let (summary, _) = &*CACHED_DIFF;

    // Use specific text to identify the advisory paragraph
    let advisory_text = "attorney licensed in the Cayman Islands";

    // The Cayman advisory should NOT be in modified_old (it doesn't exist in doc1)
    assert!(
        !summary.modified_old_contains(advisory_text),
        "Cayman advisory should NOT appear in modified old texts (it's new in doc2).\n\
         This indicates a misalignment bug.",
    );

    // The Cayman advisory should NOT be in modified_new either (it should be inserted)
    assert!(
        !summary.modified_new_contains(advisory_text),
        "Cayman advisory should NOT appear in modified new texts (it should be inserted).\n\
         This indicates the alignment is pairing it with the wrong doc1 paragraph.",
    );
}

/// Verify the diff produces a reasonable number of changes.
#[test]
fn diff_produces_changes() {
    let (summary, diff) = &*CACHED_DIFF;

    assert!(
        !diff.changes.is_empty(),
        "Diff should produce changes for US vs Cayman documents"
    );

    // Should have at least one inserted block (the Cayman advisory)
    assert!(
        !summary.inserted_texts.is_empty(),
        "Should have at least one inserted block"
    );
}

// =============================================================================
// Full document view regression tests
// =============================================================================

/// Cached full document view shared across all full_doc_* tests.
static CACHED_FULL_DOC: LazyLock<Vec<FullDocBlock>> = LazyLock::new(|| {
    let before_bytes =
        fs::read("testdata/safe-us-vs-cayman/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/safe-us-vs-cayman/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    runtime
        .full_document_view(&import_before.doc_handle, &import_after.doc_handle)
        .expect("full_document_view should succeed")
        .blocks
});

/// Helper: collect full text from inline change segments.
fn segments_text(segments: &[InlineChange]) -> String {
    let mut out = String::new();
    for seg in segments {
        match seg {
            InlineChange::Unchanged { text, .. }
            | InlineChange::Inserted { text, .. }
            | InlineChange::Deleted { text, .. } => out.push_str(text),
            InlineChange::Opaque {
                text: Some(text), ..
            } => out.push_str(text),
            InlineChange::Opaque { .. } => {}
        }
    }
    out
}

/// Helper: check if segments contain any insert or delete changes.
fn has_inline_changes(segments: &[InlineChange]) -> bool {
    segments.iter().any(|seg| {
        matches!(
            seg,
            InlineChange::Inserted { .. }
                | InlineChange::Deleted { .. }
                | InlineChange::Opaque {
                    segment_type: stemma::InlineChangeSegmentType::Insert
                        | stemma::InlineChangeSegmentType::Delete,
                    ..
                }
        )
    })
}

/// REGRESSION TEST: The Securities Act paragraph must appear as "modified" in
/// the full document view, not "unchanged".
///
/// Before (US):
///   "...securities laws of certain states...pledged or hypothecated...under the act..."
/// After (Cayman):
///   "...securities laws of any other jurisdiction...subject to security or hypothecated...under the securities act..."
///
/// These are clearly different paragraphs. If the full document view marks this
/// as "unchanged", the frontend will not render inline diffs for it.
#[test]
fn full_doc_securities_act_paragraph_is_modified() {
    let blocks = &*CACHED_FULL_DOC;

    // Find the block whose text contains the Securities Act registration language.
    // This is present in both documents but with different wording.
    let securities_block = blocks.iter().find(|b| {
        let text = segments_text(&b.segments);
        text.contains("SECURITIES ACT") && text.contains("PURSUANT HERETO")
    });

    assert!(
        securities_block.is_some(),
        "Should find the Securities Act paragraph in full document view.\n\
         Block change_types: {:?}",
        blocks
            .iter()
            .map(|b| (
                &b.change_type,
                segments_text(&b.segments)
                    .chars()
                    .take(60)
                    .collect::<String>()
            ))
            .collect::<Vec<_>>()
    );

    let block = securities_block.unwrap();
    assert_eq!(
        block.change_type,
        ChangeType::Modified,
        "Securities Act paragraph should be 'modified', not '{}'. \n\
         The US and Cayman versions have different wording \
         (e.g. 'certain states' vs 'any other jurisdiction').\n\
         Segments: {:?}",
        block.change_type,
        block.segments
    );
}

/// REGRESSION TEST: The modified Securities Act paragraph must have inline diff
/// segments showing the specific text changes.
///
/// Key differences that must appear as insert/delete segments:
/// - "certain states" (deleted) → "any other jurisdiction" (inserted)
/// - "pledged or hypothecated" (deleted) → "subject to security or hypothecated" (inserted)
/// - "under the act" (deleted) → "under the securities act" (inserted)
#[test]
fn full_doc_securities_act_has_inline_diffs() {
    let blocks = &*CACHED_FULL_DOC;

    let securities_block = blocks.iter().find(|b| {
        let text = segments_text(&b.segments);
        text.contains("SECURITIES ACT") && text.contains("PURSUANT HERETO")
    });

    let block = securities_block.expect("Securities Act paragraph should exist in full doc view");

    assert!(
        has_inline_changes(&block.segments),
        "Securities Act paragraph should have inline insert/delete segments, \
         but all segments are unchanged.\nSegments: {:?}",
        block.segments
    );

    // Collect deleted and inserted text fragments
    let deleted_texts: Vec<&str> = block
        .segments
        .iter()
        .filter_map(|seg| match seg {
            InlineChange::Deleted { text, .. } => Some(text.as_str()),
            InlineChange::Opaque {
                segment_type: stemma::InlineChangeSegmentType::Delete,
                text,
                ..
            } => text.as_deref(),
            _ => None,
        })
        .collect();

    let inserted_texts: Vec<&str> = block
        .segments
        .iter()
        .filter_map(|seg| match seg {
            InlineChange::Inserted { text, .. } => Some(text.as_str()),
            InlineChange::Opaque {
                segment_type: stemma::InlineChangeSegmentType::Insert,
                text,
                ..
            } => text.as_deref(),
            _ => None,
        })
        .collect();

    let all_deleted = deleted_texts.join("");
    let all_inserted = inserted_texts.join("");

    // "CERTAIN STATES" should be deleted (US version, caps-formatted)
    assert!(
        all_deleted.to_uppercase().contains("CERTAIN")
            && all_deleted.to_uppercase().contains("STATES"),
        "Should detect deletion of 'certain states' (may be caps-formatted).\nDeleted: {:?}",
        deleted_texts
    );

    // "any other jurisdiction" should be inserted (Cayman version)
    assert!(
        all_inserted.to_uppercase().contains("JURISDICTION"),
        "Should detect insertion of 'any other jurisdiction' (may be caps-formatted).\nInserted: {:?}",
        inserted_texts
    );
}

// =============================================================================
// doc1_block_id / doc2_block_id tests
// =============================================================================

/// All block_id values must be unique (global sequential counter).
#[test]
fn full_doc_block_ids_are_unique() {
    let blocks = &*CACHED_FULL_DOC;
    let mut seen = std::collections::HashSet::new();
    for block in blocks {
        assert!(
            seen.insert(&block.block_id),
            "Duplicate block_id: {:?}",
            block.block_id
        );
    }
}

/// block_id values follow the expected format: `p_N` for blocks present in
/// the target document, `deleted:p_N` for blocks only in the base document.
///
/// In a diff view the numbering is not guaranteed to be strictly sequential
/// by array index because: (a) canonical IDs are 1-based, and (b) deleted
/// blocks interleave with target-side blocks. What matters is uniqueness
/// (tested by `full_doc_block_ids_are_unique`) and correct format.
#[test]
fn full_doc_block_ids_have_valid_format() {
    let blocks = &*CACHED_FULL_DOC;
    for block in blocks.iter() {
        let id = &*block.block_id.0;
        let valid = id
            .strip_prefix("deleted:p_")
            .or_else(|| id.strip_prefix("p_"))
            .and_then(|suffix| suffix.parse::<u32>().ok())
            .is_some();
        assert!(
            valid,
            "block_id {:?} does not match expected format p_N or deleted:p_N",
            block.block_id
        );
    }
}

/// Inserted blocks must have doc1_block_id=None, doc2_block_id=Some.
#[test]
fn full_doc_inserted_blocks_have_correct_doc_ids() {
    let blocks = &*CACHED_FULL_DOC;
    for block in blocks
        .iter()
        .filter(|b| b.change_type == ChangeType::Inserted)
    {
        assert!(
            block.doc1_block_id.is_none(),
            "Inserted block {:?} should have doc1_block_id=None, got {:?}",
            block.block_id,
            block.doc1_block_id
        );
        assert!(
            block.doc2_block_id.is_some(),
            "Inserted block {:?} should have doc2_block_id=Some, got None",
            block.block_id
        );
    }
}

/// Deleted blocks must have doc1_block_id=Some, doc2_block_id=None.
#[test]
fn full_doc_deleted_blocks_have_correct_doc_ids() {
    let blocks = &*CACHED_FULL_DOC;
    for block in blocks
        .iter()
        .filter(|b| b.change_type == ChangeType::Deleted)
    {
        assert!(
            block.doc1_block_id.is_some(),
            "Deleted block {:?} should have doc1_block_id=Some, got None",
            block.block_id
        );
        assert!(
            block.doc2_block_id.is_none(),
            "Deleted block {:?} should have doc2_block_id=None, got {:?}",
            block.block_id,
            block.doc2_block_id
        );
    }
}

/// Modified blocks must have both doc1_block_id and doc2_block_id set.
#[test]
fn full_doc_modified_blocks_have_both_doc_ids() {
    let blocks = &*CACHED_FULL_DOC;
    for block in blocks
        .iter()
        .filter(|b| b.change_type == ChangeType::Modified)
    {
        assert!(
            block.doc1_block_id.is_some(),
            "Modified block {:?} should have doc1_block_id=Some, got None",
            block.block_id
        );
        assert!(
            block.doc2_block_id.is_some(),
            "Modified block {:?} should have doc2_block_id=Some, got None",
            block.block_id
        );
    }
}

/// Unchanged blocks must have both doc1_block_id and doc2_block_id set.
#[test]
fn full_doc_unchanged_blocks_have_both_doc_ids() {
    let blocks = &*CACHED_FULL_DOC;
    for block in blocks
        .iter()
        .filter(|b| b.change_type == ChangeType::Unchanged)
    {
        assert!(
            block.doc1_block_id.is_some(),
            "Unchanged block {:?} should have doc1_block_id=Some, got None",
            block.block_id
        );
        assert!(
            block.doc2_block_id.is_some(),
            "Unchanged block {:?} should have doc2_block_id=Some, got None",
            block.block_id
        );
    }
}

// =============================================================================
// Header diff regression tests
// =============================================================================
//
// ## Root cause analysis
//
// In the DOCX files for this comparison:
//
// BEFORE: Default -> header1.xml = "Version 1.2\nPOST-MONEY VALUATION CAP"
//         First(1)-> header2.xml = "Version 1.2\nPOST-MONEY VALUATION CAP"
//         First(2)-> header3.xml = "" (empty, second section)
//
// AFTER:  First(1)-> header1.xml = "Cayman Version 1.2\nPOST-MONEY VALUATION CAP"
//         Default -> header2.xml = "" (EMPTY!)
//         First(2)-> header3.xml = "" (empty, second section)
//
// The header content moved from Default to First between documents, and the
// content was also modified ("Cayman " prefix added).
//
// `unique_by_kind` drops kinds that appear more than once (First appears 2x in
// both docs), so ONLY the Default kind is compared. But Default went from
// content to empty, producing only BlockDeleted with no new text.
//
// The user sees the header change as: "Version 1.2" → "Cayman Version 1.2"
// but the diff shows the old header entirely deleted with no replacement.

/// All header text from either document must be accounted for in the diff.
///
/// Collect all text from all header DiffChanges. Any non-empty header content
/// that exists in either document must appear in either old_text or new_text
/// of some header change — it must not be silently dropped.
///
/// REGRESSION: `unique_by_kind` drops headers whose kind appears more than
/// once (multi-section ambiguity). When the substantive header content lives
/// in a duplicated kind (First), it is entirely skipped. Meanwhile the Default
/// kind matches to an empty header, producing only deletes.
///
/// Before: Default header = "Version 1.2\nPOST-MONEY VALUATION CAP"
/// After:  First header   = "Cayman Version 1.2\nPOST-MONEY VALUATION CAP"
///         (Default header is empty in the after document)
///
/// The diff must surface "Cayman Version 1.2" somewhere — not silently drop it.
#[test]
fn header_diff_surfaces_all_content() {
    let (_, diff) = &*CACHED_DIFF;

    // Collect all old and new text across ALL header changes
    let mut all_old = Vec::new();
    let mut all_new = Vec::new();

    for change in &diff.changes {
        match change {
            DiffChange::HeaderModified { block_changes, .. } => {
                for bc in block_changes {
                    match bc {
                        DiffChange::BlockModified {
                            old_text, new_text, ..
                        } => {
                            all_old.push(old_text.clone());
                            all_new.push(new_text.clone());
                        }
                        DiffChange::BlockDeleted { old_text, .. } => {
                            all_old.push(old_text.clone());
                        }
                        DiffChange::BlockInserted { block, .. } => {
                            all_new.push(block_text(block));
                        }
                        _ => {}
                    }
                }
            }
            DiffChange::HeaderDeleted { blocks, .. } => {
                for b in blocks {
                    let t = block_text(b);
                    if !t.is_empty() {
                        all_old.push(t);
                    }
                }
            }
            DiffChange::HeaderInserted { blocks, .. } => {
                for b in blocks {
                    let t = block_text(b);
                    if !t.is_empty() {
                        all_new.push(t);
                    }
                }
            }
            _ => {}
        }
    }

    let old_combined = all_old.join("\n");
    let new_combined = all_new.join("\n");

    // The before header text must appear in old changes
    assert!(
        old_combined.contains("Version 1.2"),
        "Before header text 'Version 1.2' must appear in old header changes.\n\
         Old texts: {:?}",
        all_old
    );

    // CRITICAL: The after header text must appear in new changes.
    // This is the core regression — "Cayman Version 1.2" was silently dropped
    // because the First kind was filtered by unique_by_kind.
    assert!(
        new_combined.contains("Cayman"),
        "After header text 'Cayman Version 1.2' must appear in new header changes.\n\
         The diff silently dropped the after document's header content.\n\
         New texts: {:?}\n\
         Old texts: {:?}",
        all_new,
        all_old
    );
}

#[test]
fn cayman_redline_keeps_structural_markers_outside_tracked_change_containers() {
    let redline = generate_redline_docx();
    let findings = tracked_container_descendants_with_structural_markers(&redline);

    assert!(
        findings.is_empty(),
        "tracked-change containers must not contain paragraph-level structural markers; found {:?}",
        findings
    );
}

#[test]
fn cayman_redline_drops_deleted_auto_field_shell_from_footer1() {
    let redline = generate_redline_docx();
    let footer_xml = read_part_xml_from_docx(&redline, "word/footer1.xml");
    let root = Element::parse(Cursor::new(footer_xml)).expect("parse footer1.xml");
    let mut findings = Vec::new();
    deleted_field_shell_descendants(&root, &mut findings);

    assert!(
        findings.is_empty(),
        "deleted auto field shell must not survive in footer1.xml; found {:?}",
        findings
    );
}

#[test]
fn cayman_redline_never_writes_synthesized_blank_header_parts() {
    // The importer's §17.10.5 blank default header is DERIVED view-state: the
    // source docx never authored it, so the redline output must not invent a
    // part for it (the old orphan "synthesized-blank-header-default.xml" was a
    // validator-visible wart), nor a relationship pointing at it.
    let redline = generate_redline_docx();
    let mut zip = ZipArchive::new(Cursor::new(&redline[..])).expect("open zip");
    for index in 0..zip.len() {
        let name = zip.by_index(index).expect("zip entry").name().to_string();
        assert!(
            !name.contains("synthesized-blank"),
            "no synthesized blank part may be written; found {name}"
        );
    }
    let rels = read_part_xml_from_docx(&redline, "word/_rels/document.xml.rels");
    assert!(
        !rels.contains("synthesized-blank"),
        "no relationship may target a synthesized blank part; rels: {rels}"
    );
}
