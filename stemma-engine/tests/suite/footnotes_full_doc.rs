//! Regression tests for footnote references in full document view.
//!
//! Uses the `footnotes` testdata which contains documents where a footnote
//! reference is inserted into an existing paragraph.
//!
//! ## Bug this tests for:
//!
//! When a paragraph gains a footnote reference (or any opaque inline),
//! `build_full_document_view` produced a full paragraph delete+insert instead
//! of a granular inline diff showing only the footnote as inserted. This was
//! caused by a blanket `has_opaque` guard that skipped token-level diffing
//! whenever either side contained any opaque inline.

use std::fs;

use stemma::{
    ChangeType, DocxRuntime, FullDocBlock, InlineChange, InlineChangeSegmentType,
    OpaqueSegmentKind, SimpleRuntime,
};

// =============================================================================
// Helpers
// =============================================================================

fn load_full_document_view() -> Vec<FullDocBlock> {
    let before_bytes = fs::read("testdata/footnotes/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/footnotes/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    runtime
        .full_document_view(&import_before.doc_handle, &import_after.doc_handle)
        .expect("full_document_view should succeed")
        .blocks
}

/// Find blocks that contain at least one `InlineChange::Opaque` with FootnoteReference kind.
fn blocks_with_footnote_ref(blocks: &[FullDocBlock]) -> Vec<&FullDocBlock> {
    blocks
        .iter()
        .filter(|b| {
            b.segments.iter().any(|s| {
                matches!(
                    s,
                    InlineChange::Opaque {
                        kind: OpaqueSegmentKind::FootnoteReference,
                        ..
                    }
                )
            })
        })
        .collect()
}

// =============================================================================
// Tests
// =============================================================================

/// The footnotes document should produce at least one block with a footnote
/// reference opaque segment.
#[test]
fn has_footnote_reference_segments() {
    let blocks = load_full_document_view();
    let fn_blocks = blocks_with_footnote_ref(&blocks);
    assert!(
        !fn_blocks.is_empty(),
        "Expected at least one block with a FootnoteReference opaque segment.\n\
         Total blocks: {}",
        blocks.len(),
    );
}

/// REGRESSION TEST: Inserting a footnote reference into a paragraph should
/// produce a granular diff — not a full paragraph delete+insert.
///
/// Before the fix, `build_full_document_view` had a blanket `has_opaque` guard
/// that fell back to `build_full_replace` whenever either side of a modified
/// block contained any opaque inline. This produced a full paragraph delete
/// followed by a full paragraph insert, losing the granular diff.
///
/// After the fix, the modified block should contain:
/// - `InlineChange::Unchanged` segments for the shared text
/// - `InlineChange::Opaque { segment_type: Insert, kind: FootnoteReference }`
///   for the inserted footnote reference
/// - NO full-paragraph delete+insert pattern (which would manifest as every
///   text segment being either Deleted or Inserted with no Unchanged segments)
#[test]
fn footnote_insert_produces_granular_diff() {
    let blocks = load_full_document_view();

    // Find modified blocks that have a footnote reference
    let modified_with_fn: Vec<_> = blocks
        .iter()
        .filter(|b| {
            b.change_type == ChangeType::Modified
                && b.segments.iter().any(|s| {
                    matches!(
                        s,
                        InlineChange::Opaque {
                            kind: OpaqueSegmentKind::FootnoteReference,
                            ..
                        }
                    )
                })
        })
        .collect();

    assert!(
        !modified_with_fn.is_empty(),
        "Expected at least one modified block with a FootnoteReference segment.\n\
         All blocks with footnotes: {:?}",
        blocks_with_footnote_ref(&blocks)
            .iter()
            .map(|b| (&b.block_id, &b.change_type))
            .collect::<Vec<_>>(),
    );

    for block in &modified_with_fn {
        // The block must have at least one Unchanged segment — proving we got
        // a granular diff rather than a full replace.
        let has_unchanged = block
            .segments
            .iter()
            .any(|s| matches!(s, InlineChange::Unchanged { .. }));

        assert!(
            has_unchanged,
            "Modified block {:?} with footnote reference has no Unchanged segments.\n\
             This means the diff fell back to full paragraph replace instead of \
             granular inline diff.\nSegments: {:?}",
            block.block_id, block.segments,
        );

        // The footnote reference should be marked as Insert (it was added)
        let has_inserted_fn = block.segments.iter().any(|s| {
            matches!(
                s,
                InlineChange::Opaque {
                    segment_type: InlineChangeSegmentType::Insert,
                    kind: OpaqueSegmentKind::FootnoteReference,
                    ..
                }
            )
        });

        assert!(
            has_inserted_fn,
            "Modified block {:?} should have an inserted FootnoteReference segment.\n\
             Segments: {:?}",
            block.block_id, block.segments,
        );
    }
}

/// Modified blocks with footnote references must not have the full-replace
/// pattern where ALL text segments are deleted+inserted (no unchanged text).
#[test]
fn no_full_replace_pattern_with_footnotes() {
    let blocks = load_full_document_view();

    let modified_with_fn: Vec<_> = blocks
        .iter()
        .filter(|b| {
            b.change_type == ChangeType::Modified
                && b.segments.iter().any(|s| {
                    matches!(
                        s,
                        InlineChange::Opaque {
                            kind: OpaqueSegmentKind::FootnoteReference,
                            ..
                        }
                    )
                })
        })
        .collect();

    for block in &modified_with_fn {
        // Count text segments (non-opaque) by type
        let text_segments: Vec<_> = block
            .segments
            .iter()
            .filter(|s| !matches!(s, InlineChange::Opaque { .. }))
            .collect();

        if text_segments.is_empty() {
            continue; // Pure opaque block, nothing to check
        }

        let all_deleted_or_inserted = text_segments.iter().all(|s| {
            matches!(
                s,
                InlineChange::Deleted { .. } | InlineChange::Inserted { .. }
            )
        });

        assert!(
            !all_deleted_or_inserted,
            "Modified block {:?} has the full-replace pattern: all text segments are \
             Deleted or Inserted with no Unchanged text. This indicates the opaque \
             guard fell back to build_full_replace.\nSegments: {:?}",
            block.block_id, block.segments,
        );
    }
}
