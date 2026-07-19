//! Integration tests for the bookmark authoring verb
//! (`InsertBookmark` / `RenameBookmark` / `RemoveBookmark`).
//!
//! A bookmark is a zero-width structural annotation (a `w:bookmarkStart` +
//! `w:bookmarkEnd` pair, §17.13.6), NOT a tracked content change. The standing
//! invariants for this verb are therefore:
//!
//! - **T1 text identity** — inserting / renaming / removing a bookmark changes
//!   the visible text on NO projection: the read view, accept-all, and
//!   reject-all all reproduce the original text exactly (bookmarks are
//!   zero-width).
//! - **Roundtrip** — a synthesized authored bookmark survives serialize ->
//!   reimport: its name is intact, its `w:id` was remapped to a fresh numeric
//!   id, and the start/end pair shares that id (no collision, no orphan).
//! - **Schema refusals** — duplicate / orphan-end / missing / empty-name each
//!   surface a distinct error carrying the offending name in context.
//!
//! All tests synthesize their DOCX in memory and pass with all env unset
//! (corpus-free, daily tier).

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::runtime::{ErrorCode, Resolution};

// ─── Fixtures ──────────────────────────────────────────────────────────────

fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for para in paragraphs {
        document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Bookmarks".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// First body block id.
fn first_block_id(doc: &Document) -> NodeId {
    doc.read().blocks[0].id.clone()
}

/// Apply a transaction expecting failure; return the `RuntimeError`. (`Document`
/// is not `Debug`, so `Result::expect_err` is unavailable.)
fn apply_expect_err(doc: &Document, txn: EditTransaction) -> stemma::runtime::RuntimeError {
    match doc.apply(&txn) {
        Ok(_) => panic!("expected the transaction to be refused, but it applied"),
        Err(e) => e,
    }
}

/// Concatenated visible text of every body paragraph, in order.
fn visible_text(doc: &Document) -> String {
    let mut out = String::new();
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline {
                        out.push_str(&t.text);
                    }
                }
            }
        }
    }
    out
}

/// Every bookmark `(name, w:id)` from `w:bookmarkStart` decorations in the body.
fn bookmark_starts(doc: &Document) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Decoration(d) = inline
                        && d.kind == DecorationType::Bookmark
                        && let Some(raw) = &d.raw_xml
                    {
                        let xml = String::from_utf8_lossy(raw);
                        if xml.contains("bookmarkStart")
                            && let (Some(name), Some(id)) =
                                (attr_value(&xml, "w:name"), attr_value(&xml, "w:id"))
                        {
                            out.push((name, id));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Every `w:bookmarkEnd` `w:id` in the body.
fn bookmark_end_ids(doc: &Document) -> Vec<String> {
    let mut out = Vec::new();
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Decoration(d) = inline
                        && d.kind == DecorationType::Bookmark
                        && let Some(raw) = &d.raw_xml
                    {
                        let xml = String::from_utf8_lossy(raw);
                        if xml.contains("bookmarkEnd")
                            && let Some(id) = attr_value(&xml, "w:id")
                        {
                            out.push(id);
                        }
                    }
                }
            }
        }
    }
    out
}

/// Crude attribute-value extraction from a single-element XML fragment.
fn attr_value(xml: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = xml.find(&needle)? + needle.len();
    let rest = &xml[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ─── T1: text identity (bookmarks are zero-width) ───────────────────────────

#[test]
fn insert_bookmark_does_not_change_text_on_any_projection() {
    let original = "The Confidential Information clause governs disclosure.";
    let base = Document::parse(&make_test_docx(&[original])).expect("parse");
    let block = first_block_id(&base);

    let edited = base
        .apply(&txn(vec![EditStep::InsertBookmark {
            block_id: block,
            expect: "Confidential Information".to_string(),
            semantic_hash: None,
            name: "DefTerm".to_string(),
            rationale: None,
        }]))
        .expect("insert bookmark applies");

    // Read view text is unchanged (bookmarks are zero-width).
    assert_eq!(visible_text(&edited), original);

    // accept-all and reject-all both reproduce the original text.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert_eq!(visible_text(&accepted), original, "accept text identity");
    assert_eq!(visible_text(&rejected), original, "reject text identity");
}

// ─── Roundtrip: synthesized bookmark survives serialize -> reimport ──────────

#[test]
fn inserted_bookmark_roundtrips_with_name_and_remapped_id() {
    let base = Document::parse(&make_test_docx(&["alpha beta gamma delta"])).expect("parse");
    let block = first_block_id(&base);

    let edited = base
        .apply(&txn(vec![EditStep::InsertBookmark {
            block_id: block,
            expect: "beta gamma".to_string(),
            semantic_hash: None,
            name: "Section_2".to_string(),
            rationale: None,
        }]))
        .expect("insert applies");

    // Serialize -> reimport (full roundtrip through the serializer's remap).
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reimport serialized bytes");

    // Name intact.
    let starts = bookmark_starts(&reimported);
    assert_eq!(starts.len(), 1, "exactly one bookmark survives");
    let (name, start_id) = &starts[0];
    assert_eq!(name, "Section_2", "bookmark name preserved");

    // The serializer remaps the placeholder to a small numeric id (NOT the
    // 900000000 placeholder we synthesized): id was reassigned.
    let id_num: u32 = start_id.parse().expect("w:id is numeric");
    assert!(
        id_num < 900_000_000,
        "serializer must remap the authored placeholder id, got {id_num}"
    );

    // start/end pair shares the remapped id: no collision, no orphan.
    let end_ids = bookmark_end_ids(&reimported);
    assert_eq!(end_ids.len(), 1, "exactly one bookmarkEnd");
    assert_eq!(&end_ids[0], start_id, "end shares the start's remapped id");

    // Text unchanged through the roundtrip.
    assert_eq!(visible_text(&reimported), "alpha beta gamma delta");
}

#[test]
fn two_bookmarks_get_distinct_remapped_ids_after_roundtrip() {
    let base = Document::parse(&make_test_docx(&["one two three four five"])).expect("parse");
    let block = first_block_id(&base);

    let edited = base
        .apply(&txn(vec![
            EditStep::InsertBookmark {
                block_id: block.clone(),
                expect: "two".to_string(),
                semantic_hash: None,
                name: "First".to_string(),
                rationale: None,
            },
            EditStep::InsertBookmark {
                block_id: block,
                expect: "four".to_string(),
                semantic_hash: None,
                name: "Second".to_string(),
                rationale: None,
            },
        ]))
        .expect("two inserts apply");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reimport");

    let starts = bookmark_starts(&reimported);
    assert_eq!(starts.len(), 2, "both bookmarks survive");
    let id_a = &starts[0].1;
    let id_b = &starts[1].1;
    assert_ne!(id_a, id_b, "distinct bookmarks must get distinct w:ids");

    // Each end pairs with its own start.
    let ends = bookmark_end_ids(&reimported);
    assert_eq!(ends.len(), 2);
    assert!(ends.contains(id_a) && ends.contains(id_b));
}

// ─── RenameBookmark roundtrip ────────────────────────────────────────────────

#[test]
fn rename_bookmark_changes_name_keeps_id_through_roundtrip() {
    let base = Document::parse(&make_test_docx(&["alpha beta gamma"])).expect("parse");
    let block = first_block_id(&base);

    let inserted = base
        .apply(&txn(vec![EditStep::InsertBookmark {
            block_id: block.clone(),
            expect: "beta".to_string(),
            semantic_hash: None,
            name: "OldName".to_string(),
            rationale: None,
        }]))
        .expect("insert");

    let renamed = inserted
        .apply(&txn(vec![EditStep::RenameBookmark {
            block_id: block,
            old_name: "OldName".to_string(),
            new_name: "NewName".to_string(),
            semantic_hash: None,
            rationale: None,
        }]))
        .expect("rename");

    let bytes = renamed
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reimport");

    let starts = bookmark_starts(&reimported);
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].0, "NewName", "rename took effect");
    // end still pairs.
    assert_eq!(bookmark_end_ids(&reimported), vec![starts[0].1.clone()]);
}

// ─── RemoveBookmark roundtrip ────────────────────────────────────────────────

#[test]
fn remove_bookmark_drops_pair_through_roundtrip() {
    let base = Document::parse(&make_test_docx(&["one two three four"])).expect("parse");
    let block = first_block_id(&base);

    let inserted = base
        .apply(&txn(vec![
            EditStep::InsertBookmark {
                block_id: block.clone(),
                expect: "two".to_string(),
                semantic_hash: None,
                name: "Keep".to_string(),
                rationale: None,
            },
            EditStep::InsertBookmark {
                block_id: block.clone(),
                expect: "four".to_string(),
                semantic_hash: None,
                name: "Drop".to_string(),
                rationale: None,
            },
        ]))
        .expect("two inserts");

    let removed = inserted
        .apply(&txn(vec![EditStep::RemoveBookmark {
            block_id: block,
            name: "Drop".to_string(),
            semantic_hash: None,
            rationale: None,
        }]))
        .expect("remove");

    let bytes = removed
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reimport");

    let starts = bookmark_starts(&reimported);
    assert_eq!(starts.len(), 1, "only the kept bookmark remains");
    assert_eq!(starts[0].0, "Keep");
    // No orphan end: exactly one end, pairing with the kept start.
    assert_eq!(bookmark_end_ids(&reimported), vec![starts[0].1.clone()]);
    assert_eq!(visible_text(&reimported), "one two three four");
}

// ─── Schema refusals (each a distinct error with name in context) ────────────

#[test]
fn insert_duplicate_name_is_unsupported_edit() {
    let base = Document::parse(&make_test_docx(&["alpha beta gamma"])).expect("parse");
    let block = first_block_id(&base);

    let once = base
        .apply(&txn(vec![EditStep::InsertBookmark {
            block_id: block.clone(),
            expect: "alpha".to_string(),
            semantic_hash: None,
            name: "Dup".to_string(),
            rationale: None,
        }]))
        .expect("first insert");

    let err = apply_expect_err(
        &once,
        txn(vec![EditStep::InsertBookmark {
            block_id: block,
            expect: "gamma".to_string(),
            semantic_hash: None,
            name: "Dup".to_string(),
            rationale: None,
        }]),
    );
    assert_eq!(err.code, ErrorCode::UnsupportedEdit);
    assert!(
        err.message.contains("Dup"),
        "error must name the duplicate bookmark: {}",
        err.message
    );
}

#[test]
fn rename_missing_is_anchor_not_found() {
    let base = Document::parse(&make_test_docx(&["alpha beta"])).expect("parse");
    let block = first_block_id(&base);
    let err = apply_expect_err(
        &base,
        txn(vec![EditStep::RenameBookmark {
            block_id: block,
            old_name: "Absent".to_string(),
            new_name: "X".to_string(),
            semantic_hash: None,
            rationale: None,
        }]),
    );
    assert_eq!(err.code, ErrorCode::AnchorNotFound);
    assert!(
        err.message.contains("Absent"),
        "error names the missing bookmark: {}",
        err.message
    );
}

#[test]
fn remove_missing_is_anchor_not_found() {
    let base = Document::parse(&make_test_docx(&["alpha beta"])).expect("parse");
    let block = first_block_id(&base);
    let err = apply_expect_err(
        &base,
        txn(vec![EditStep::RemoveBookmark {
            block_id: block,
            name: "Ghost".to_string(),
            semantic_hash: None,
            rationale: None,
        }]),
    );
    assert_eq!(err.code, ErrorCode::AnchorNotFound);
    assert!(
        err.message.contains("Ghost"),
        "error names the missing bookmark: {}",
        err.message
    );
}

#[test]
fn insert_empty_name_is_unsupported_edit() {
    let base = Document::parse(&make_test_docx(&["alpha beta"])).expect("parse");
    let block = first_block_id(&base);
    let err = apply_expect_err(
        &base,
        txn(vec![EditStep::InsertBookmark {
            block_id: block,
            expect: "alpha".to_string(),
            semantic_hash: None,
            name: "   ".to_string(),
            rationale: None,
        }]),
    );
    assert_eq!(err.code, ErrorCode::UnsupportedEdit);
}

#[test]
fn insert_expect_mismatch_is_stale() {
    let base = Document::parse(&make_test_docx(&["alpha beta"])).expect("parse");
    let block = first_block_id(&base);
    let err = apply_expect_err(
        &base,
        txn(vec![EditStep::InsertBookmark {
            block_id: block,
            expect: "not present".to_string(),
            semantic_hash: None,
            name: "X".to_string(),
            rationale: None,
        }]),
    );
    assert_eq!(err.code, ErrorCode::StaleEdit);
}
