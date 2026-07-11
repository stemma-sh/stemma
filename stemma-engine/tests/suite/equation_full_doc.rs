//! Regression tests for equation (OMML) support in full document view.
//!
//! Uses the `image-math-combined` testdata which contains documents with
//! images and math equations. Tests verify that:
//!
//! 1. Changed equations produce Deleted + Inserted block pairs (not Modified).
//!    m:oMathPara cannot appear inside w:del/w:ins, so the merge path splits
//!    changed equations into separate blocks for correct redline serialization.
//! 2. Deleted/Inserted equation blocks report `content_types` including "equation".
//! 3. `equation_xmls` contains raw OMML XML for the equation in each block.
//! 4. `equation_doc1_count` correctly reflects the source of equation XMLs.

use std::fs;

use stemma::{ChangeType, DocxRuntime, FullDocBlock, SimpleRuntime};

// =============================================================================
// Helpers
// =============================================================================

fn load_full_document_view() -> Vec<FullDocBlock> {
    let before_bytes =
        fs::read("testdata/image-math-combined/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/image-math-combined/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    runtime
        .full_document_view(&import_before.doc_handle, &import_after.doc_handle)
        .expect("full_document_view should succeed")
        .blocks
}

/// Find blocks where content_types includes "equation".
fn equation_blocks(blocks: &[FullDocBlock]) -> Vec<&FullDocBlock> {
    blocks
        .iter()
        .filter(|b| b.content_types.contains(&"equation".to_string()))
        .collect()
}

// =============================================================================
// Tests
// =============================================================================

/// Changed equations produce Deleted + Inserted blocks (not Modified).
/// m:oMathPara cannot appear inside w:del/w:ins at the inline level, so
/// equation changes are tracked at the paragraph level via block splits.
#[test]
fn changed_equations_produce_deleted_and_inserted_blocks() {
    let blocks = load_full_document_view();
    let eq_blocks = equation_blocks(&blocks);

    let deleted = eq_blocks
        .iter()
        .filter(|b| b.change_type == ChangeType::Deleted)
        .count();
    let inserted = eq_blocks
        .iter()
        .filter(|b| b.change_type == ChangeType::Inserted)
        .count();

    assert!(
        deleted >= 1,
        "Expected at least one deleted equation block, found {deleted}.\n\
         Equation blocks: {eq_blocks:?}",
    );
    assert!(
        inserted >= 1,
        "Expected at least one inserted equation block, found {inserted}.\n\
         Equation blocks: {eq_blocks:?}",
    );
}

/// Deleted equation blocks must have content_types including "equation"
/// and equation_xmls populated from doc1.
#[test]
fn deleted_equation_has_content_types_and_xmls() {
    let blocks = load_full_document_view();
    let eq_blocks = equation_blocks(&blocks);

    let deleted = eq_blocks
        .iter()
        .find(|b| b.change_type == ChangeType::Deleted);

    let block = deleted.expect("Should find a deleted equation block");

    assert!(
        block.content_types.contains(&"equation".to_string()),
        "Deleted equation block {:?} should have 'equation' in content_types.\n\
         content_types: {:?}",
        block.block_id,
        block.content_types,
    );

    assert!(
        !block.equation_xmls.is_empty(),
        "Deleted equation block {:?} should have equation_xmls from doc1.",
        block.block_id,
    );

    assert_eq!(
        block.equation_doc1_count,
        block.equation_xmls.len(),
        "Deleted equation block {:?}: all equation_xmls should be from doc1.\n\
         equation_xmls: {}, equation_doc1_count: {}",
        block.block_id,
        block.equation_xmls.len(),
        block.equation_doc1_count,
    );
}

/// An inserted equation block must have equation_doc1_count == 0.
#[test]
fn inserted_equation_has_zero_doc1_count() {
    let blocks = load_full_document_view();
    let eq_blocks = equation_blocks(&blocks);

    let inserted = eq_blocks
        .iter()
        .find(|b| b.change_type == ChangeType::Inserted);

    if let Some(block) = inserted {
        assert_eq!(
            block.equation_doc1_count,
            0,
            "Inserted equation block {:?} should have equation_doc1_count == 0.\n\
             equation_xmls: {}, equation_doc1_count: {}",
            block.block_id,
            block.equation_xmls.len(),
            block.equation_doc1_count,
        );

        assert!(
            !block.equation_xmls.is_empty(),
            "Inserted equation block {:?} should have equation_xmls from doc2.",
            block.block_id,
        );
    }
}

/// Every equation block must have valid OMML XML in equation_xmls.
/// Each entry should start with an XML declaration or an `m:oMath` element.
#[test]
fn equation_xmls_contain_valid_omml() {
    let blocks = load_full_document_view();
    let eq_blocks = equation_blocks(&blocks);

    assert!(
        !eq_blocks.is_empty(),
        "Should have at least one equation block"
    );

    for block in &eq_blocks {
        for (i, xml) in block.equation_xmls.iter().enumerate() {
            assert!(
                !xml.is_empty(),
                "equation_xmls[{i}] in block {:?} is empty",
                block.block_id,
            );
            // OMML XML should contain math namespace elements
            assert!(
                xml.contains("oMath") || xml.contains("m:oMath"),
                "equation_xmls[{i}] in block {:?} doesn't look like OMML XML.\n\
                 First 200 chars: {}",
                block.block_id,
                &xml[..xml.len().min(200)],
            );
        }
    }
}

// =============================================================================
// Image tests — verify image_data_uris extraction from the same testdata
// =============================================================================

/// Find blocks where content_types includes "image".
fn image_blocks(blocks: &[FullDocBlock]) -> Vec<&FullDocBlock> {
    blocks
        .iter()
        .filter(|b| b.content_types.contains(&"image".to_string()))
        .collect()
}

/// There should be image blocks in the test documents.
#[test]
fn has_image_blocks() {
    let blocks = load_full_document_view();
    let img_blocks = image_blocks(&blocks);
    assert!(
        !img_blocks.is_empty(),
        "Expected at least one block with content_types containing 'image', found none"
    );
}

/// Image blocks should have non-empty image_data_uris.
#[test]
fn image_blocks_have_data_uris() {
    let blocks = load_full_document_view();
    let img_blocks = image_blocks(&blocks);
    for block in &img_blocks {
        assert!(
            !block.image_data_uris.is_empty(),
            "Block {:?} has content_types {:?} but image_data_uris is empty.\n\
             change_type: {}",
            block.block_id,
            block.content_types,
            block.change_type,
        );
    }
}

/// Image data URIs should be valid data URI format.
#[test]
fn image_data_uris_are_valid() {
    let blocks = load_full_document_view();
    for block in &blocks {
        for (i, uri) in block.image_data_uris.iter().enumerate() {
            assert!(
                uri.starts_with("data:image/"),
                "Block {:?}: image_data_uris[{i}] doesn't start with 'data:image/'.\n\
                 First 50 chars: {}",
                block.block_id,
                &uri[..uri.len().min(50)],
            );
            assert!(
                uri.contains(";base64,"),
                "Block {:?}: image_data_uris[{i}] missing ';base64,' marker.",
                block.block_id,
            );
        }
    }
}

/// The doc1/doc2 split must be consistent: image_doc1_count <= image_data_uris.len().
#[test]
fn image_doc1_count_is_valid_split() {
    let blocks = load_full_document_view();
    for block in &blocks {
        assert!(
            block.image_doc1_count <= block.image_data_uris.len(),
            "Block {:?}: image_doc1_count ({}) exceeds image_data_uris length ({}).\n\
             change_type: {}, content_types: {:?}",
            block.block_id,
            block.image_doc1_count,
            block.image_data_uris.len(),
            block.change_type,
            block.content_types,
        );
    }
}

/// REGRESSION TEST: Pure image blocks p_3 and p_12 contain identical images
/// between the two documents. After the content_hash fix (hashing image bytes
/// instead of raw XML bytes), these must be classified as "unchanged", not
/// falsely as "modified" due to non-deterministic XML attribute order.
#[test]
fn identical_images_are_unchanged() {
    let blocks = load_full_document_view();
    let img_blocks = image_blocks(&blocks);

    // p_3 and p_12 are pure image blocks with identical images in both docs
    let pure_image_ids = ["p_3", "p_12"];
    for expected_id in &pure_image_ids {
        let block = img_blocks
            .iter()
            .find(|b| &*b.block_id.0 == *expected_id)
            .unwrap_or_else(|| panic!("Expected image block {expected_id} not found"));

        assert_eq!(
            block.change_type,
            ChangeType::Unchanged,
            "Image block {expected_id} contains identical images in both docs but got \
             change_type='{}'. This means content_hash is still based on XML bytes \
             instead of image bytes.",
            block.change_type,
        );
    }
}

/// Unchanged image blocks should have empty image_metadata_changes.
#[test]
fn unchanged_images_have_empty_metadata_changes() {
    let blocks = load_full_document_view();
    let img_blocks = image_blocks(&blocks);

    for block in &img_blocks {
        if block.change_type == ChangeType::Unchanged {
            assert!(
                block.image_metadata_changes.is_empty(),
                "Unchanged image block {:?} should have no metadata changes, got {:?}",
                block.block_id,
                block.image_metadata_changes,
            );
        }
    }
}

/// The doc1/doc2 split must be consistent: equation_doc1_count <= equation_xmls.len().
#[test]
fn equation_doc1_count_is_valid_split() {
    let blocks = load_full_document_view();

    for block in &blocks {
        assert!(
            block.equation_doc1_count <= block.equation_xmls.len(),
            "Block {:?}: equation_doc1_count ({}) exceeds equation_xmls length ({}).\n\
             change_type: {}, content_types: {:?}",
            block.block_id,
            block.equation_doc1_count,
            block.equation_xmls.len(),
            block.change_type,
            block.content_types,
        );
    }
}
