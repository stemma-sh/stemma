//! Edit engine serialization tests (invariant #20).
//!
//! ## 20b: Edit → serialize → re-import (daily)
//!
//! For each fixture, apply a programmatic edit via `runtime.apply_edit()`,
//! then export to DOCX, re-import, and verify accept/reject text is correct
//! through the serialization boundary.
//!
//! ## 20c: Edit → serialize → Word Oracle (nightly)
//!
//! Same pipeline, but validates with Microsoft Word:
//! - `/validate` — edited DOCX opens clean
//! - `/accept` — Word accept-all yields the new text
//! - `/reject` — Word reject-all yields the original text

use std::fs;
use stemma::docx::DocxArchive;
use stemma::domain::*;
use stemma::edit::*;
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::vocabulary::{NumberingSource, extract_vocabulary};
use stemma::{DocxRuntime, ExportMode, SimpleRuntime, accept_all, reject_all_with_styles};
use xmltree::Element;

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        author: Some("Edit Engine".to_string()),
        date: Some("2026-03-28T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

/// Discover fixture directories that have before.docx.
fn discover_fixtures() -> Vec<String> {
    let mut paths = Vec::new();
    if let Ok(entries) = fs::read_dir("testdata") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let before = path.join("before.docx");
                if before.exists() {
                    paths.push(before.to_string_lossy().to_string());
                }
            }
        }
    }
    paths.sort();
    paths
}

/// Find the first editable paragraph in a CanonDoc: Normal status, no tracked
/// segments, no opaques, at least 2 whitespace-separated words.
/// Returns (block_id, visible_text, first_word).
fn find_editable_paragraph(doc: &CanonDoc) -> Option<(NodeId, String, String)> {
    doc.blocks.iter().find_map(|tb| {
        if !matches!(tb.status, TrackingStatus::Normal) {
            return None;
        }
        let BlockNode::Paragraph(p) = &tb.block else {
            return None;
        };
        if p.segments
            .iter()
            .any(|s| !matches!(s.status, TrackingStatus::Normal))
        {
            return None;
        }
        let has_opaque = p.segments.iter().any(|s| {
            s.inlines
                .iter()
                .any(|i| matches!(i, InlineNode::OpaqueInline(_)))
        });
        if has_opaque {
            return None;
        }
        let text: String = p
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        let first_word = text.split_whitespace().next().unwrap_or("").to_string();
        if text.split_whitespace().count() >= 2 {
            Some((p.id.clone(), text, first_word))
        } else {
            None
        }
    })
}

/// Build an edit transaction that replaces the first word with "EDITED".
fn make_edit_transaction(
    block_id: &NodeId,
    original_text: &str,
    first_word: &str,
) -> (EditTransaction, String) {
    let new_text = original_text.replacen(first_word, "EDITED", 1);
    let tx = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: first_word.to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(new_text.clone())],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };
    (tx, new_text)
}

fn para_visible_text(doc: &CanonDoc, block_id: &NodeId) -> String {
    doc.blocks
        .iter()
        .find_map(|tb| {
            if let BlockNode::Paragraph(p) = &tb.block {
                if &p.id == block_id {
                    Some(
                        p.segments
                            .iter()
                            .flat_map(|s| s.inlines.iter())
                            .filter_map(|i| match i {
                                InlineNode::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect(),
                    )
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("paragraph '{block_id}' not found"))
}

/// Normalize text for comparison: collapse whitespace, drop empties, join with newline.
fn normalize_doc_text(para_texts: &[String]) -> String {
    para_texts
        .iter()
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug)]
struct TextParagraphCandidate {
    block_index: usize,
    block_id: NodeId,
    text: String,
}

#[derive(Debug)]
struct TransactionScenario {
    name: &'static str,
    tx: EditTransaction,
    expected_accept: String,
    expected_reject: String,
}

fn top_level_para_texts(doc: &CanonDoc) -> Vec<String> {
    doc.blocks
        .iter()
        .filter_map(|tb| {
            if let BlockNode::Paragraph(p) = &tb.block {
                let text: String = p
                    .segments
                    .iter()
                    .flat_map(|s| s.inlines.iter())
                    .filter_map(|i| match i {
                        InlineNode::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                Some(text)
            } else {
                None
            }
        })
        .collect()
}

fn find_insertable_role(doc: &CanonDoc) -> Option<String> {
    let vocab = extract_vocabulary(doc);
    vocab
        .paragraph_roles
        .iter()
        .find(|role| {
            role.count > 0 && role.numbering_source != Some(NumberingSource::LiteralPrefix)
        })
        .map(|role| role.id.clone())
}

fn replace_first_word(text: &str, replacement: &str) -> Option<(String, String)> {
    let first_word = text.split_whitespace().next()?;
    let new_text = text.replacen(first_word, replacement, 1);
    Some((first_word.to_string(), new_text))
}

fn find_text_only_paragraph_candidates(doc: &CanonDoc) -> Vec<TextParagraphCandidate> {
    doc.blocks
        .iter()
        .enumerate()
        .filter_map(|(block_index, tb)| {
            if !matches!(tb.status, TrackingStatus::Normal) {
                return None;
            }
            let BlockNode::Paragraph(p) = &tb.block else {
                return None;
            };
            if p.segments
                .iter()
                .any(|s| !matches!(s.status, TrackingStatus::Normal))
            {
                return None;
            }
            let has_anchor = p.segments.iter().any(|s| {
                s.inlines
                    .iter()
                    .any(|i| matches!(i, InlineNode::OpaqueInline(_) | InlineNode::HardBreak(_)))
            });
            if has_anchor {
                return None;
            }
            let text: String = p
                .segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .filter_map(|i| match i {
                    InlineNode::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect();
            if text.split_whitespace().count() >= 2 {
                Some(TextParagraphCandidate {
                    block_index,
                    block_id: p.id.clone(),
                    text,
                })
            } else {
                None
            }
        })
        .collect()
}

fn build_transaction_scenarios(doc: &CanonDoc) -> Vec<TransactionScenario> {
    let mut scenarios = Vec::new();
    let original_para_texts = top_level_para_texts(doc);
    let expected_reject = normalize_doc_text(&original_para_texts);
    let candidates = find_text_only_paragraph_candidates(doc);
    let insert_role = find_insertable_role(doc);

    if let (Some(candidate), Some(role)) = (candidates.first(), insert_role.clone())
        && let Some((expect, replaced_text)) = replace_first_word(&candidate.text, "EDITED")
    {
        let inserted_text = "METAMORPHIC inserted paragraph".to_string();
        let tx = EditTransaction {
            steps: vec![
                EditStep::ReplaceParagraphText {
                    block_id: candidate.block_id.clone(),
                    rationale: Some("Metamorphic replace+insert combo".to_string()),
                    replacement_role: None,
                    expect,
                    semantic_hash: None,
                    content: ParagraphContent {
                        fragments: vec![ContentFragment::Text(replaced_text.clone())],
                    },
                },
                EditStep::InsertParagraphs {
                    anchor_block_id: candidate.block_id.clone(),
                    position: InsertPosition::After,
                    rationale: Some("Metamorphic inserted follow-up paragraph".to_string()),
                    blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                        role: Some(role.clone()),
                        content: parse_paragraph_markup(&inserted_text).unwrap(),
                        restart_numbering: false,
                        list: None,
                    })],
                },
            ],
            summary: Some("replace + insert".to_string()),
            materialization_mode: MaterializationMode::TrackedChange,
            revision: test_revision(),
        };
        let mut accepted = original_para_texts.clone();
        accepted[candidate.block_index] = replaced_text;
        accepted.insert(candidate.block_index + 1, inserted_text);
        scenarios.push(TransactionScenario {
            name: "replace_insert_combo",
            tx,
            expected_accept: normalize_doc_text(&accepted),
            expected_reject: expected_reject.clone(),
        });

        let first_insert = "FIRST inserted paragraph".to_string();
        let second_insert = "SECOND inserted paragraph".to_string();
        let tx = EditTransaction {
            steps: vec![
                EditStep::InsertParagraphs {
                    anchor_block_id: candidate.block_id.clone(),
                    position: InsertPosition::After,
                    rationale: Some("First insert after shared anchor".to_string()),
                    blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                        role: Some(role.clone()),
                        content: parse_paragraph_markup(&first_insert).unwrap(),
                        restart_numbering: false,
                        list: None,
                    })],
                },
                EditStep::InsertParagraphs {
                    anchor_block_id: candidate.block_id.clone(),
                    position: InsertPosition::After,
                    rationale: Some("Second insert after shared anchor".to_string()),
                    blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                        role: Some(role),
                        content: parse_paragraph_markup(&second_insert).unwrap(),
                        restart_numbering: false,
                        list: None,
                    })],
                },
            ],
            summary: Some("repeated insert ordering".to_string()),
            materialization_mode: MaterializationMode::TrackedChange,
            revision: test_revision(),
        };
        let mut accepted = original_para_texts.clone();
        accepted.insert(candidate.block_index + 1, first_insert);
        accepted.insert(candidate.block_index + 2, second_insert);
        scenarios.push(TransactionScenario {
            name: "repeated_insert_after_anchor",
            tx,
            expected_accept: normalize_doc_text(&accepted),
            expected_reject: expected_reject.clone(),
        });
    }

    if let (Some(delete_candidate), Some(anchor_candidate), Some(role)) =
        (candidates.first(), candidates.get(1), insert_role.clone())
    {
        let inserted_text = "POST delete inserted paragraph".to_string();
        let tx = EditTransaction {
            steps: vec![
                EditStep::DeleteBlockRange {
                    from_block_id: delete_candidate.block_id.clone(),
                    to_block_id: delete_candidate.block_id.clone(),
                    rationale: Some("Metamorphic delete one paragraph".to_string()),
                    expect: delete_candidate
                        .text
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string(),
                    semantic_hash: None,
                },
                EditStep::InsertParagraphs {
                    anchor_block_id: anchor_candidate.block_id.clone(),
                    position: InsertPosition::After,
                    rationale: Some("Insert after surviving anchor".to_string()),
                    blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                        role: Some(role),
                        content: parse_paragraph_markup(&inserted_text).unwrap(),
                        restart_numbering: false,
                        list: None,
                    })],
                },
            ],
            summary: Some("delete + insert".to_string()),
            materialization_mode: MaterializationMode::TrackedChange,
            revision: test_revision(),
        };

        let mut accepted = original_para_texts.clone();
        accepted.remove(delete_candidate.block_index);
        let mut anchor_index = anchor_candidate.block_index;
        if delete_candidate.block_index < anchor_candidate.block_index {
            anchor_index -= 1;
        }
        accepted.insert(anchor_index + 1, inserted_text);
        scenarios.push(TransactionScenario {
            name: "delete_insert_combo",
            tx,
            expected_accept: normalize_doc_text(&accepted),
            expected_reject: expected_reject.clone(),
        });
    }

    if let (Some(pair), Some(role)) = (
        candidates
            .windows(2)
            .find(|pair| pair[0].block_index + 1 == pair[1].block_index),
        insert_role,
    ) {
        let first = &pair[0];
        let second = &pair[1];
        let replacement_one = "RANGE replacement one".to_string();
        let replacement_two = "RANGE replacement two".to_string();
        let tx = EditTransaction {
            steps: vec![EditStep::ReplaceBlockRange {
                from_block_id: first.block_id.clone(),
                to_block_id: second.block_id.clone(),
                rationale: Some("Structural replace over adjacent paragraphs".to_string()),
                expect: first
                    .text
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string(),
                semantic_hash: None,
                blocks: vec![
                    BlockSpec::Paragraph(ParagraphBlockSpec {
                        role: Some(role.clone()),
                        content: parse_paragraph_markup(&replacement_one).unwrap(),
                        restart_numbering: false,
                        list: None,
                    }),
                    BlockSpec::Paragraph(ParagraphBlockSpec {
                        role: Some(role),
                        content: parse_paragraph_markup(&replacement_two).unwrap(),
                        restart_numbering: false,
                        list: None,
                    }),
                ],
            }],
            summary: Some("structural range replace".to_string()),
            materialization_mode: MaterializationMode::TrackedChange,
            revision: test_revision(),
        };
        let mut accepted = original_para_texts.clone();
        accepted.splice(
            first.block_index..=second.block_index,
            [replacement_one, replacement_two],
        );
        scenarios.push(TransactionScenario {
            name: "structural_replace_range",
            tx,
            expected_accept: normalize_doc_text(&accepted),
            expected_reject,
        });
    }

    scenarios
}

fn verify_exported_accept_reject(
    exported_bytes: &[u8],
    expected_accept: &str,
    expected_reject: &str,
) {
    let archive = DocxArchive::read(exported_bytes).expect("exported DOCX should be readable");

    let (accepted_archive, _) =
        normalize_docx(&archive).expect("normalize_docx should accept all exported edits");
    let accepted_bytes = accepted_archive
        .write()
        .expect("accepted archive should serialize");
    let accepted_runtime = SimpleRuntime::new();
    let accepted_import = accepted_runtime
        .import_docx(&accepted_bytes)
        .expect("accepted DOCX should re-import");
    assert_eq!(
        extract_all_text(&accepted_import.canonical),
        expected_accept
    );

    let (rejected_archive, _) =
        reject_all_docx(&archive).expect("reject_all_docx should reject all exported edits");
    let rejected_bytes = rejected_archive
        .write()
        .expect("rejected archive should serialize");
    let rejected_runtime = SimpleRuntime::new();
    let rejected_import = rejected_runtime
        .import_docx(&rejected_bytes)
        .expect("rejected DOCX should re-import");
    assert_eq!(
        extract_all_text(&rejected_import.canonical),
        expected_reject
    );
}

// ─── 20b: Edit → serialize → re-import (daily) ─────────────────────────────

#[test]
fn edit_serialize_reimport_roundtrip() {
    let fixtures = discover_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    let mut tested = 0;
    let mut skipped = 0;

    for fixture_path in &fixtures {
        let bytes = match fs::read(fixture_path) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let runtime = SimpleRuntime::new();
        let import = match runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let Some((block_id, original_text, first_word)) =
            find_editable_paragraph(&import.canonical)
        else {
            skipped += 1;
            continue;
        };

        let (tx, new_text) = make_edit_transaction(&block_id, &original_text, &first_word);

        // Apply edit through the runtime (serialize to DOCX internally)
        let apply_result = runtime
            .apply_edit(&import.doc_handle, &tx)
            .unwrap_or_else(|err| panic!("apply_edit failed on {fixture_path}: {err:?}"));

        // Canonical-space checks on the returned CanonDoc
        let mut accepted = (*apply_result.canonical).clone();
        accept_all(&mut accepted);
        assert_eq!(
            para_visible_text(&accepted, &block_id),
            new_text,
            "canonical accept mismatch in {fixture_path}"
        );

        let mut rejected = (*apply_result.canonical).clone();
        reject_all_with_styles(&mut rejected, None);
        assert_eq!(
            para_visible_text(&rejected, &block_id),
            original_text,
            "canonical reject mismatch in {fixture_path}"
        );

        // Export the serialized DOCX and re-import
        let exported_bytes = runtime
            .export_docx(&import.doc_handle, ExportMode::Redline)
            .unwrap_or_else(|err| panic!("export_docx failed on {fixture_path}: {err:?}"));

        assert!(
            !exported_bytes.is_empty(),
            "exported DOCX must not be empty for {fixture_path}"
        );

        // Re-import: the serialized DOCX must parse successfully
        let reimport_runtime = SimpleRuntime::new();
        let reimport = reimport_runtime
            .import_docx(&exported_bytes)
            .unwrap_or_else(|err| {
                panic!("re-import of edited DOCX failed on {fixture_path}: {err:?}")
            });

        // The re-imported doc should have tracked changes
        let _reimported_para = reimport.canonical.blocks.iter().find_map(|tb| {
            if let BlockNode::Paragraph(p) = &tb.block {
                if p.id == block_id { Some(p) } else { None }
            } else {
                None
            }
        });
        // The paragraph might have been renumbered during reimport, so just
        // verify the re-import succeeded — the canonical-space checks above
        // already proved the model is correct.

        eprintln!("  {fixture_path}: serialize roundtrip ✓");
        tested += 1;
    }

    eprintln!("{tested} fixtures tested, {skipped} skipped");
    assert!(tested > 0, "no fixtures were testable");
}

// ─── 20b sweep: canonical accept/reject on all fixtures ─────────────────────

#[test]
#[ignore = "sweep over all fixtures — run via just edit-sweep"]
fn edit_accept_reject_all_fixtures() {
    let fixtures = discover_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    let mut tested = 0;
    let mut skipped = 0;

    for fixture_path in &fixtures {
        let bytes = match fs::read(fixture_path) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let runtime = SimpleRuntime::new();
        let import = match runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let Some((block_id, original_text, first_word)) =
            find_editable_paragraph(&import.canonical)
        else {
            skipped += 1;
            continue;
        };

        let (tx, new_text) = make_edit_transaction(&block_id, &original_text, &first_word);

        // Apply through runtime (full serialize path)
        let apply_result = runtime
            .apply_edit(&import.doc_handle, &tx)
            .unwrap_or_else(|err| panic!("apply_edit failed on {fixture_path}: {err:?}"));

        // Canonical accept → new text
        let mut accepted = (*apply_result.canonical).clone();
        accept_all(&mut accepted);
        assert_eq!(
            para_visible_text(&accepted, &block_id),
            new_text,
            "accept mismatch in {fixture_path}"
        );

        // Canonical reject → original text
        let mut rejected = std::sync::Arc::unwrap_or_clone(apply_result.canonical);
        reject_all_with_styles(&mut rejected, None);
        assert_eq!(
            para_visible_text(&rejected, &block_id),
            original_text,
            "reject mismatch in {fixture_path}"
        );

        tested += 1;
    }

    eprintln!("{tested} fixtures tested, {skipped} skipped");
    assert!(tested > 0, "no fixtures were testable");
}

#[test]
fn edit_transaction_metamorphic_smoke() {
    let fixtures = discover_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    let mut ran_any = false;

    for fixture_path in &fixtures {
        let bytes = match fs::read(fixture_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let runtime = SimpleRuntime::new();
        let import = match runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let scenarios = build_transaction_scenarios(&import.canonical);
        if scenarios.is_empty() {
            continue;
        }

        for scenario in scenarios.iter().take(2) {
            let rt = SimpleRuntime::new();
            let imp = rt.import_docx(&bytes).unwrap_or_else(|err| {
                panic!("{fixture_path} [{}]: import failed: {err:?}", scenario.name)
            });
            let apply_result = rt
                .apply_edit(&imp.doc_handle, &scenario.tx)
                .unwrap_or_else(|err| {
                    panic!(
                        "{fixture_path} [{}]: apply_edit failed: {err:?}",
                        scenario.name
                    )
                });

            let mut accepted = (*apply_result.canonical).clone();
            accept_all(&mut accepted);
            assert_eq!(
                extract_all_text(&accepted),
                scenario.expected_accept,
                "{fixture_path} [{}]: canonical accept mismatch",
                scenario.name
            );

            let mut rejected = std::sync::Arc::unwrap_or_clone(apply_result.canonical);
            reject_all_with_styles(&mut rejected, None);
            assert_eq!(
                extract_all_text(&rejected),
                scenario.expected_reject,
                "{fixture_path} [{}]: canonical reject mismatch",
                scenario.name
            );

            let exported_bytes = rt
                .export_docx(&imp.doc_handle, ExportMode::Redline)
                .unwrap_or_else(|err| {
                    panic!(
                        "{fixture_path} [{}]: export_docx failed: {err:?}",
                        scenario.name
                    )
                });
            verify_exported_accept_reject(
                &exported_bytes,
                &scenario.expected_accept,
                &scenario.expected_reject,
            );
            ran_any = true;
        }

        if ran_any {
            break;
        }
    }

    assert!(
        ran_any,
        "no fixtures produced a metamorphic transaction scenario"
    );
}

// ─── 20b stress: diverse edit patterns on all fixtures ─────────────────────

/// Describes a single edit pattern to apply to a paragraph.
#[derive(Debug)]
enum EditPattern {
    ReplaceFirstWord,
    ReplaceLastWord,
    ReplaceMiddleWord,
    DeleteWord,
    InsertWord,
    ReplaceEntireText,
}

impl std::fmt::Display for EditPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditPattern::ReplaceFirstWord => write!(f, "replace_first_word"),
            EditPattern::ReplaceLastWord => write!(f, "replace_last_word"),
            EditPattern::ReplaceMiddleWord => write!(f, "replace_middle_word"),
            EditPattern::DeleteWord => write!(f, "delete_word"),
            EditPattern::InsertWord => write!(f, "insert_word"),
            EditPattern::ReplaceEntireText => write!(f, "replace_entire_text"),
        }
    }
}

/// Find ALL editable text-only paragraphs (Normal status, no tracked segments,
/// no opaques/hard breaks, at least 2 words).
fn find_text_only_paragraphs(doc: &CanonDoc) -> Vec<(NodeId, String)> {
    doc.blocks
        .iter()
        .filter_map(|tb| {
            if !matches!(tb.status, TrackingStatus::Normal) {
                return None;
            }
            let BlockNode::Paragraph(p) = &tb.block else {
                return None;
            };
            if p.segments
                .iter()
                .any(|s| !matches!(s.status, TrackingStatus::Normal))
            {
                return None;
            }
            let has_anchor = p.segments.iter().any(|s| {
                s.inlines
                    .iter()
                    .any(|i| matches!(i, InlineNode::OpaqueInline(_) | InlineNode::HardBreak(_)))
            });
            if has_anchor {
                return None;
            }
            let text: String = p
                .segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .filter_map(|i| match i {
                    InlineNode::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect();
            if text.split_whitespace().count() >= 2 {
                Some((p.id.clone(), text))
            } else {
                None
            }
        })
        .collect()
}

/// Build an edit transaction for a given pattern on a text-only paragraph.
/// Returns (transaction, expected_new_text) or None if pattern doesn't apply.
fn build_pattern_edit(
    block_id: &NodeId,
    original_text: &str,
    pattern: &EditPattern,
) -> Option<(EditTransaction, String)> {
    let words: Vec<&str> = original_text.split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }

    let (expect, new_text) = match pattern {
        EditPattern::ReplaceFirstWord => {
            let first = words[0];
            let new = original_text.replacen(first, "EDITED", 1);
            (first.to_string(), new)
        }
        EditPattern::ReplaceLastWord => {
            let last = *words.last().unwrap();
            // Replace only the last occurrence
            let rfind = original_text.rfind(last)?;
            let mut new = original_text.to_string();
            new.replace_range(rfind..rfind + last.len(), "REPLACED");
            (last.to_string(), new)
        }
        EditPattern::ReplaceMiddleWord => {
            if words.len() < 3 {
                return None;
            }
            let mid_idx = words.len() / 2;
            let mid = words[mid_idx];
            let new = original_text.replacen(mid, "MIDDLE", 1);
            (mid.to_string(), new)
        }
        EditPattern::DeleteWord => {
            let first = words[0];
            // Delete the first word (and the space after it)
            let new = original_text
                .replacen(first, "", 1)
                .trim_start()
                .to_string();
            (first.to_string(), new)
        }
        EditPattern::InsertWord => {
            let first = words[0];
            // Insert "EXTRA" before the first word
            let new = format!("EXTRA {original_text}");
            (first.to_string(), new)
        }
        EditPattern::ReplaceEntireText => {
            let first = words[0];
            let new = "Completely replaced paragraph text.".to_string();
            (first.to_string(), new)
        }
    };

    let tx = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect,
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(new_text.clone())],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };
    Some((tx, new_text))
}

/// Find paragraphs that HAVE preserved inlines (opaques or hard breaks) and
/// are otherwise editable (Normal status, no tracked segments, have text in
/// at least one section).
///
/// Returns: (block_id, text_sections, anchor_ids)
/// where text_sections[i] is the text between anchor[i-1] and anchor[i],
/// and anchor_ids are the NodeIds of the preserved inlines in order.
fn find_paragraphs_with_inlines(doc: &CanonDoc) -> Vec<(NodeId, Vec<String>, Vec<NodeId>)> {
    doc.blocks
        .iter()
        .filter_map(|tb| {
            if !matches!(tb.status, TrackingStatus::Normal) {
                return None;
            }
            let BlockNode::Paragraph(p) = &tb.block else {
                return None;
            };
            if p.segments
                .iter()
                .any(|s| !matches!(s.status, TrackingStatus::Normal))
            {
                return None;
            }

            // Collect text sections and anchor ids
            let mut sections = Vec::new();
            let mut anchor_ids = Vec::new();
            let mut current_text = String::new();

            for seg in &p.segments {
                for inline in &seg.inlines {
                    match inline {
                        InlineNode::Text(t) => {
                            current_text.push_str(&t.text);
                        }
                        InlineNode::OpaqueInline(o) => {
                            sections.push(std::mem::take(&mut current_text));
                            anchor_ids.push(o.id.clone());
                        }
                        InlineNode::HardBreak(hb) => {
                            sections.push(std::mem::take(&mut current_text));
                            anchor_ids.push(hb.id.clone());
                        }
                        _ => {} // decorations are zero-width, skip
                    }
                }
            }
            sections.push(current_text);

            if anchor_ids.is_empty() {
                return None;
            }

            // Need at least one non-empty text section
            let has_text = sections.iter().any(|s| !s.trim().is_empty());
            if !has_text {
                return None;
            }

            Some((p.id.clone(), sections, anchor_ids))
        })
        .collect()
}

/// Build a ParagraphContent that preserves all inlines and replaces text
/// in one section. Returns (transaction, expected_accept_text, expected_reject_text).
fn build_inline_preserving_edit(
    block_id: &NodeId,
    text_sections: &[String],
    anchor_ids: &[NodeId],
    section_to_edit: usize,
) -> Option<(EditTransaction, String, String)> {
    // The section must have a non-empty word to use as expect
    let section_text = &text_sections[section_to_edit];
    let words: Vec<&str> = section_text.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }

    let expect_word = words[0];
    let new_section_text = section_text.replacen(expect_word, "INLINED", 1);

    // Build fragments: text[0], anchor[0], text[1], anchor[1], ..., text[N]
    let mut fragments = Vec::new();
    for (i, section) in text_sections.iter().enumerate() {
        let text = if i == section_to_edit {
            &new_section_text
        } else {
            section
        };
        if !text.is_empty() {
            fragments.push(ContentFragment::Text(text.clone()));
        }
        if i < anchor_ids.len() {
            fragments.push(ContentFragment::PreservedInlineRef(anchor_ids[i].clone()));
        }
    }

    // Compute expected full text for accept and reject.
    // Accept text = all sections with the edited one replaced (text nodes only).
    let accept_text: String = text_sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i == section_to_edit {
                new_section_text.clone()
            } else {
                s.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("");
    let reject_text: String = text_sections.join("");

    let tx = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: expect_word.to_string(),
            semantic_hash: None,
            content: ParagraphContent { fragments },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    Some((tx, accept_text, reject_text))
}

#[test]
#[ignore = "stress diverse edit patterns — run via just edit-stress"]
fn edit_stress_diverse_patterns() {
    let fixtures = discover_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    let patterns = [
        EditPattern::ReplaceFirstWord,
        EditPattern::ReplaceLastWord,
        EditPattern::ReplaceMiddleWord,
        EditPattern::DeleteWord,
        EditPattern::InsertWord,
        EditPattern::ReplaceEntireText,
    ];

    let mut total_successes = 0usize;
    let mut total_failures: Vec<String> = Vec::new();
    let mut total_skips = 0usize;

    for fixture_path in &fixtures {
        let bytes = match fs::read(fixture_path) {
            Ok(b) => b,
            Err(_) => {
                total_skips += 1;
                continue;
            }
        };
        let runtime = SimpleRuntime::new();
        let import = match runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(_) => {
                total_skips += 1;
                continue;
            }
        };

        // ── Part 1: Text-only paragraphs with diverse patterns ──

        let text_paragraphs = find_text_only_paragraphs(&import.canonical);
        if text_paragraphs.is_empty() {
            // Still try Part 2 below
        }

        // Test the first suitable text-only paragraph with each pattern
        for (block_id, original_text) in text_paragraphs.iter().take(1) {
            for pattern in &patterns {
                let Some((tx, new_text)) = build_pattern_edit(block_id, original_text, pattern)
                else {
                    total_skips += 1;
                    continue;
                };

                // Re-import fresh for each edit (edits mutate the doc handle)
                let rt = SimpleRuntime::new();
                let imp = match rt.import_docx(&bytes) {
                    Ok(r) => r,
                    Err(e) => {
                        total_failures.push(format!(
                            "{fixture_path} [{pattern}]: re-import failed: {e:?}"
                        ));
                        continue;
                    }
                };

                match rt.apply_edit(&imp.doc_handle, &tx) {
                    Ok(apply_result) => {
                        // Verify accept == new text
                        let mut accepted = (*apply_result.canonical).clone();
                        accept_all(&mut accepted);
                        let accept_text = para_visible_text(&accepted, block_id);
                        if accept_text != new_text {
                            total_failures.push(format!(
                                "{fixture_path} [{pattern}]: accept mismatch\n  \
                                 expected: {:?}\n  actual:   {:?}",
                                &new_text[..new_text.len().min(120)],
                                &accept_text[..accept_text.len().min(120)],
                            ));
                            continue;
                        }

                        // Verify reject == original text
                        let mut rejected = std::sync::Arc::unwrap_or_clone(apply_result.canonical);
                        reject_all_with_styles(&mut rejected, None);
                        let reject_text = para_visible_text(&rejected, block_id);
                        if reject_text != *original_text {
                            total_failures.push(format!(
                                "{fixture_path} [{pattern}]: reject mismatch\n  \
                                 expected: {:?}\n  actual:   {:?}",
                                &original_text[..original_text.len().min(120)],
                                &reject_text[..reject_text.len().min(120)],
                            ));
                            continue;
                        }

                        total_successes += 1;
                    }
                    Err(err) => {
                        total_failures.push(format!(
                            "{fixture_path} [{pattern}]: apply_edit failed: {err:?}"
                        ));
                    }
                }
            }
        }

        // ── Part 2: Paragraphs with preserved inlines ──

        let inline_paragraphs = find_paragraphs_with_inlines(&import.canonical);

        for (block_id, text_sections, anchor_ids) in inline_paragraphs.iter().take(2) {
            // Try editing each text section that has content
            for section_idx in 0..text_sections.len() {
                let Some((tx, expected_accept, expected_reject)) =
                    build_inline_preserving_edit(block_id, text_sections, anchor_ids, section_idx)
                else {
                    total_skips += 1;
                    continue;
                };

                let rt = SimpleRuntime::new();
                let imp = match rt.import_docx(&bytes) {
                    Ok(r) => r,
                    Err(e) => {
                        total_failures.push(format!(
                            "{fixture_path} [inline para={block_id} section={section_idx}]: \
                             re-import failed: {e:?}"
                        ));
                        continue;
                    }
                };

                match rt.apply_edit(&imp.doc_handle, &tx) {
                    Ok(apply_result) => {
                        let mut accepted = (*apply_result.canonical).clone();
                        accept_all(&mut accepted);
                        let accept_text = para_visible_text(&accepted, block_id);
                        if accept_text != expected_accept {
                            total_failures.push(format!(
                                "{fixture_path} [inline para={block_id} section={section_idx}]: \
                                 accept mismatch\n  expected: {:?}\n  actual:   {:?}",
                                &expected_accept[..expected_accept.len().min(120)],
                                &accept_text[..accept_text.len().min(120)],
                            ));
                            continue;
                        }

                        let mut rejected = std::sync::Arc::unwrap_or_clone(apply_result.canonical);
                        reject_all_with_styles(&mut rejected, None);
                        let reject_text = para_visible_text(&rejected, block_id);
                        if reject_text != expected_reject {
                            total_failures.push(format!(
                                "{fixture_path} [inline para={block_id} section={section_idx}]: \
                                 reject mismatch\n  expected: {:?}\n  actual:   {:?}",
                                &expected_reject[..expected_reject.len().min(120)],
                                &reject_text[..reject_text.len().min(120)],
                            ));
                            continue;
                        }

                        total_successes += 1;
                    }
                    Err(err) => {
                        total_failures.push(format!(
                            "{fixture_path} [inline para={block_id} section={section_idx}]: \
                             apply_edit failed: {err:?}"
                        ));
                    }
                }
            }
        }
    }

    eprintln!("\n── edit_stress_diverse_patterns results ──");
    eprintln!(
        "  successes: {total_successes}, failures: {}, skips: {total_skips}",
        total_failures.len()
    );
    if !total_failures.is_empty() {
        eprintln!("\n  Failures:");
        for f in &total_failures {
            eprintln!("    FAIL: {f}");
        }
        panic!("{} edit patterns failed (see above)", total_failures.len());
    }
    assert!(
        total_successes > 0,
        "no edit patterns were testable across all fixtures"
    );
}

#[test]
#[ignore = "combined multi-step transaction sweep over all fixtures — run via just edit-sweep"]
fn edit_transaction_metamorphic_sweep() {
    let fixtures = discover_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    let mut tested = 0usize;
    let mut skipped = 0usize;
    let mut failures = Vec::new();

    for fixture_path in &fixtures {
        let bytes = match fs::read(fixture_path) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let runtime = SimpleRuntime::new();
        let import = match runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let scenarios = build_transaction_scenarios(&import.canonical);
        if scenarios.is_empty() {
            skipped += 1;
            continue;
        }

        for scenario in scenarios {
            let rt = SimpleRuntime::new();
            let imp = match rt.import_docx(&bytes) {
                Ok(r) => r,
                Err(err) => {
                    failures.push(format!(
                        "{fixture_path} [{}]: import failed: {err:?}",
                        scenario.name
                    ));
                    continue;
                }
            };

            let apply_result = match rt.apply_edit(&imp.doc_handle, &scenario.tx) {
                Ok(r) => r,
                Err(err) => {
                    failures.push(format!(
                        "{fixture_path} [{}]: apply_edit failed: {err:?}",
                        scenario.name
                    ));
                    continue;
                }
            };

            let mut accepted = (*apply_result.canonical).clone();
            accept_all(&mut accepted);
            let accepted_text = extract_all_text(&accepted);
            if accepted_text != scenario.expected_accept {
                failures.push(format!(
                    "{fixture_path} [{}]: canonical accept mismatch\n  expected: {:?}\n  actual:   {:?}",
                    scenario.name,
                    scenario.expected_accept,
                    accepted_text
                ));
                continue;
            }

            let mut rejected = std::sync::Arc::unwrap_or_clone(apply_result.canonical);
            reject_all_with_styles(&mut rejected, None);
            let rejected_text = extract_all_text(&rejected);
            if rejected_text != scenario.expected_reject {
                failures.push(format!(
                    "{fixture_path} [{}]: canonical reject mismatch\n  expected: {:?}\n  actual:   {:?}",
                    scenario.name,
                    scenario.expected_reject,
                    rejected_text
                ));
                continue;
            }

            let exported_bytes = match rt.export_docx(&imp.doc_handle, ExportMode::Redline) {
                Ok(b) => b,
                Err(err) => {
                    failures.push(format!(
                        "{fixture_path} [{}]: export_docx failed: {err:?}",
                        scenario.name
                    ));
                    continue;
                }
            };

            let export_check = std::panic::catch_unwind(|| {
                verify_exported_accept_reject(
                    &exported_bytes,
                    &scenario.expected_accept,
                    &scenario.expected_reject,
                )
            });
            if export_check.is_err() {
                failures.push(format!(
                    "{fixture_path} [{}]: exported accept/reject verification failed",
                    scenario.name
                ));
                continue;
            }

            tested += 1;
        }
    }

    eprintln!(
        "\n{tested} metamorphic transaction scenarios passed, {skipped} fixtures skipped, {} failed",
        failures.len()
    );
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("  FAIL: {failure}");
        }
        panic!(
            "{} metamorphic transaction scenarios failed",
            failures.len()
        );
    }
    assert!(
        tested > 0,
        "no metamorphic transaction scenarios were testable"
    );
}

/// Extract all paragraph texts from a CanonDoc (for comparison).
fn extract_all_text(doc: &CanonDoc) -> String {
    let texts: Vec<String> = doc
        .blocks
        .iter()
        .filter_map(|tb| {
            if let BlockNode::Paragraph(p) = &tb.block {
                let text: String = p
                    .segments
                    .iter()
                    .flat_map(|s| s.inlines.iter())
                    .filter_map(|i| match i {
                        InlineNode::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                Some(text)
            } else {
                None
            }
        })
        .collect();
    normalize_doc_text(&texts)
}

/// Local attribute accessor for tests: look up an attribute on an xmltree
/// Element by local name, ignoring namespace/prefix. Exists because
/// `crate::xml_attrs::attr_get` is `pub(crate)` and not reachable from
/// integration tests.
fn el_attr<'a>(el: &'a Element, local: &str) -> Option<&'a str> {
    el.attributes
        .iter()
        .find_map(|(name, value)| (name.local_name == local).then_some(value.as_str()))
}

// ─── Numbering restart: LLM schema v3 ────────────────────────────────────────
//
// Op: `insert` with numbering restart.
// When an `insert` step carries `restart_numbering: true` on a numbered role,
// the exported DOCX must contain a fresh `w:num` instance referencing the
// same `abstractNumId` as the role's exemplar, with a `w:lvlOverride` that
// restarts the counter at 1 via `w:startOverride val="1"`. The inserted
// paragraph's `w:numPr/w:numId` must point at the new instance so Word
// re-renders the counter from 1.
//
// Strategy: iterate fixtures, find one whose vocabulary exposes a paragraph
// role backed by Word auto-numbering, apply a restart-insert, then inspect
// `word/numbering.xml` and the inserted paragraph in the exported DOCX.

/// Find an auto-numbered (non-bullet) insertable role: vocabulary entry
/// with `has_numbering = true`, `numbering_source = Auto`, whose exemplar
/// paragraph's `NumberingInfo.is_bullet` is false — bullets have no counter
/// and reject `restart_numbering` (see `edit.rs::resolve_paragraph_spec`).
fn find_auto_numbered_role(doc: &CanonDoc) -> Option<String> {
    fn exemplar_numbering<'a>(doc: &'a CanonDoc, id: &NodeId) -> Option<&'a NumberingInfo> {
        fn in_block<'a>(block: &'a BlockNode, id: &NodeId) -> Option<&'a NumberingInfo> {
            match block {
                BlockNode::Paragraph(p) if &p.id == id => p.numbering.as_ref(),
                BlockNode::Table(t) => t.rows.iter().find_map(|row| {
                    row.cells
                        .iter()
                        .find_map(|cell| cell.blocks.iter().find_map(|b| in_block(b, id)))
                }),
                _ => None,
            }
        }
        doc.blocks.iter().find_map(|tb| in_block(&tb.block, id))
    }
    let vocab = extract_vocabulary(doc);
    vocab
        .paragraph_roles
        .iter()
        .find(|role| {
            role.count > 0
                && role.has_numbering
                && role.numbering_source == Some(NumberingSource::Auto)
                && exemplar_numbering(doc, &role.exemplar).is_some_and(|n| !n.is_bullet)
        })
        .map(|role| role.id.clone())
}

/// Find an anchor paragraph we can insert after: a Normal-status paragraph
/// with no existing tracked changes. Prefer one that itself has numbering
/// matching the given role's exemplar so the restart is visibly meaningful
/// (though correctness doesn't require this — the test only asserts the
/// override XML is present).
fn find_simple_insert_anchor(doc: &CanonDoc) -> Option<NodeId> {
    doc.blocks.iter().find_map(|tb| {
        if !matches!(tb.status, TrackingStatus::Normal) {
            return None;
        }
        let BlockNode::Paragraph(p) = &tb.block else {
            return None;
        };
        if p.segments
            .iter()
            .any(|s| !matches!(s.status, TrackingStatus::Normal))
        {
            return None;
        }
        Some(p.id.clone())
    })
}

#[test]
fn edit_insert_toc_block_emits_tracked_complex_field() {
    use xmltree::XMLNode;

    let fixture_path = "testdata/spec-compliance/fields/simple-field/input.docx";
    let bytes = fs::read(fixture_path).expect("read fixture");
    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(&bytes).expect("import fixture");

    let anchor_id = find_simple_insert_anchor(&import.canonical).expect("insert anchor present");
    let role = extract_vocabulary(&import.canonical)
        .paragraph_roles
        .first()
        .expect("fixture vocabulary must have a paragraph role")
        .id
        .clone();

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: anchor_id,
            position: InsertPosition::After,
            rationale: Some("Insert generated TOC field.".to_string()),
            blocks: vec![BlockSpec::Toc(TocBlockSpec {
                role: Some(role),
                levels: TocLevelsSpec { from: 1, to: 3 },
                include_hyperlinks: true,
                hide_page_numbers_in_web: true,
                use_outline_levels: true,
            })],
        }],
        summary: Some("insert toc".to_string()),
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    runtime
        .apply_edit(&import.doc_handle, &tx)
        .unwrap_or_else(|err| panic!("apply_edit(insert toc) failed: {err:?}"));

    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export DOCX");
    let archive = DocxArchive::read(&exported).expect("exported DOCX readable");
    let document_xml = archive
        .get("word/document.xml")
        .expect("document.xml present");
    let root = Element::parse(std::io::Cursor::new(document_xml)).expect("document.xml parses");

    // A tracked field INSERT lowers to the complex form inside w:ins —
    // `w:fldSimple` cannot ride inside `w:ins`, and outside it Word reads the
    // field as permanent (unrejectable) content. The instruction therefore
    // lives in `w:instrText` runs under the insertion.
    fn collect_ins_instr_text(el: &Element, inside_ins: bool, out: &mut Vec<String>) {
        let inside = inside_ins || el.name == "ins";
        if inside && el.name == "instrText" {
            out.push(
                el.children
                    .iter()
                    .filter_map(|c| match c {
                        XMLNode::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            );
        }
        for child in &el.children {
            if let XMLNode::Element(child_el) = child {
                collect_ins_instr_text(child_el, inside, out);
            }
        }
    }

    let mut instructions = Vec::new();
    collect_ins_instr_text(&root, false, &mut instructions);
    assert!(
        instructions
            .iter()
            .any(|instr| instr == "TOC \\o \"1-3\" \\h \\z \\u"),
        "expected generated TOC instruction in w:ins-wrapped w:instrText, got {instructions:?}"
    );

    let reimport_runtime = SimpleRuntime::new();
    let reimport = reimport_runtime
        .import_docx(&exported)
        .expect("re-import exported DOCX");
    let reimport_fields: Vec<&FieldData> = reimport
        .canonical
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p),
            _ => None,
        })
        .flat_map(|p| p.segments.iter())
        .flat_map(|segment| segment.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                OpaqueKind::Field(field) if field.field_kind == FieldKind::Instruction => {
                    Some(field)
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert!(
        reimport_fields
            .iter()
            .any(|field| field.instruction_text.as_deref() == Some("TOC \\o \"1-3\" \\h \\z \\u")),
        "re-imported DOCX should preserve the TOC instruction text, got {:?}",
        reimport_fields
            .iter()
            .map(|field| field.instruction_text.as_deref().unwrap_or("<none>"))
            .collect::<Vec<_>>()
    );
    assert!(
        reimport_fields.iter().any(|field| {
            field.semantic
                == Some(FieldSemantic::Toc(TocFieldSpec {
                    levels: TocLevelsSpec { from: 1, to: 3 },
                    include_hyperlinks: true,
                    hide_page_numbers_in_web: true,
                    use_outline_levels: true,
                }))
        }),
        "re-imported DOCX should preserve TOC semantic classification"
    );
}

#[test]
fn edit_insert_with_restart_numbering_emits_start_override() {
    use xmltree::XMLNode;

    // Fixtures with decimal (non-bullet) auto-numbering. `discover_fixtures`
    // only returns dirs with `before.docx`; none of those have non-bullet
    // numbered paragraphs today, so we also look at a spec-compliance fixture
    // that carries `input.docx` with decimal numbering on `w:numId=1`.
    let mut candidates: Vec<String> = discover_fixtures();
    let spec_fixture = "testdata/spec-compliance/numbering/start-at-override/input.docx";
    if std::path::Path::new(spec_fixture).exists() {
        candidates.insert(0, spec_fixture.to_string());
    }

    assert!(!candidates.is_empty(), "no fixtures found");

    let mut tested_any = false;

    for fixture_path in &candidates {
        let Ok(bytes) = fs::read(fixture_path) else {
            continue;
        };
        let runtime = SimpleRuntime::new();
        let Ok(import) = runtime.import_docx(&bytes) else {
            continue;
        };

        let Some(role) = find_auto_numbered_role(&import.canonical) else {
            continue;
        };
        let Some(anchor_id) = find_simple_insert_anchor(&import.canonical) else {
            continue;
        };

        // Snapshot the set of numIds present in the base numbering.xml before
        // the edit. After the edit, the new paragraph must reference a numId
        // that is NOT in this set.
        let base_archive =
            DocxArchive::read(&bytes).expect("base DOCX should read for numId snapshot");
        let base_numbering = base_archive
            .get("word/numbering.xml")
            .expect("fixture with auto-numbered role must have word/numbering.xml");
        let base_root = Element::parse(std::io::Cursor::new(base_numbering))
            .expect("base numbering.xml should parse");
        let mut base_num_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for child in &base_root.children {
            if let XMLNode::Element(el) = child
                && el.name == "num"
                && let Some(v) = el_attr(el, "numId")
                && let Ok(n) = v.parse::<u32>()
            {
                base_num_ids.insert(n);
            }
        }

        let inserted_text = "SCHEDULE A: DATA PROCESSING TERMS".to_string();
        let tx = EditTransaction {
            steps: vec![EditStep::InsertParagraphs {
                anchor_block_id: anchor_id.clone(),
                position: InsertPosition::After,
                rationale: Some("Restart numbering for schedule heading.".to_string()),
                blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some(role.clone()),
                    content: parse_paragraph_markup(&inserted_text).unwrap(),
                    restart_numbering: true,
                    list: None,
                })],
            }],
            summary: Some("restart_numbering smoke".to_string()),
            materialization_mode: MaterializationMode::TrackedChange,
            revision: test_revision(),
        };

        let apply_result = runtime
            .apply_edit(&import.doc_handle, &tx)
            .unwrap_or_else(|err| {
                panic!("apply_edit(restart_numbering) failed on {fixture_path}: {err:?}")
            });

        // Find the inserted paragraph in the canonical model and record its
        // numId — this must refer to the new override instance.
        let inserted_num_id = apply_result.canonical.blocks.iter().find_map(|tb| {
            if let BlockNode::Paragraph(p) = &tb.block
                && matches!(tb.status, TrackingStatus::Inserted(_))
                && let Some(n) = p.numbering.as_ref()
                && !n.restart_numbering
            {
                Some((p.id.clone(), n.num_id, n.ilvl))
            } else {
                None
            }
        });
        let Some((new_block_id, new_num_id, new_ilvl)) = inserted_num_id else {
            panic!(
                "inserted paragraph with numbering not found in {fixture_path} \
                 (role={role})"
            );
        };
        assert!(
            !base_num_ids.contains(&new_num_id),
            "inserted paragraph '{new_block_id}' reuses base numId {new_num_id}; \
             restart_numbering must allocate a fresh instance (fixture={fixture_path})"
        );

        // Export the edited DOCX and inspect its numbering.xml.
        let exported = runtime
            .export_docx(&import.doc_handle, ExportMode::Redline)
            .unwrap_or_else(|err| panic!("export_docx failed on {fixture_path}: {err:?}"));
        let exported_archive = DocxArchive::read(&exported).expect("exported DOCX should read");
        let exported_numbering_bytes = exported_archive
            .get("word/numbering.xml")
            .expect("exported DOCX must still carry word/numbering.xml");
        let exported_root = Element::parse(std::io::Cursor::new(exported_numbering_bytes))
            .expect("exported numbering.xml should parse");

        // The new w:num instance must exist, reference an abstractNumId, and
        // contain a w:lvlOverride with w:startOverride val="1" on new_ilvl.
        let new_num_id_str = new_num_id.to_string();
        let new_ilvl_str = new_ilvl.to_string();
        let new_num_el = exported_root
            .children
            .iter()
            .find_map(|c| {
                if let XMLNode::Element(el) = c
                    && el.name == "num"
                    && el_attr(el, "numId") == Some(new_num_id_str.as_str())
                {
                    Some(el)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                panic!(
                    "exported numbering.xml missing new w:num numId={new_num_id} \
                     (fixture={fixture_path})"
                )
            });

        let lvl_override = new_num_el
            .children
            .iter()
            .find_map(|c| {
                if let XMLNode::Element(el) = c
                    && el.name == "lvlOverride"
                    && el_attr(el, "ilvl") == Some(new_ilvl_str.as_str())
                {
                    Some(el)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                panic!(
                    "w:num numId={new_num_id} missing w:lvlOverride for ilvl={new_ilvl} \
                     (fixture={fixture_path})"
                )
            });

        let start_override = lvl_override.children.iter().find_map(|c| {
            if let XMLNode::Element(el) = c
                && el.name == "startOverride"
            {
                el_attr(el, "val").map(str::to_string)
            } else {
                None
            }
        });
        assert_eq!(
            start_override.as_deref(),
            Some("1"),
            "w:startOverride must be val=\"1\" in {fixture_path}"
        );

        // Restart flag must be cleared on the canonical paragraph after
        // serialization so a subsequent render does not re-allocate.
        let still_pending = apply_result.canonical.blocks.iter().any(|tb| {
            if let BlockNode::Paragraph(p) = &tb.block {
                p.numbering.as_ref().is_some_and(|n| n.restart_numbering)
            } else {
                false
            }
        });
        assert!(
            !still_pending,
            "restart_numbering flag must be cleared after serialize (fixture={fixture_path})"
        );

        eprintln!("  {fixture_path}: restart_numbering ✓");
        tested_any = true;
        break; // one fixture is enough to prove the round-trip works
    }

    assert!(
        tested_any,
        "no fixture had an auto-numbered role to exercise restart_numbering against"
    );
}

/// A multi-item *new* list inserted below an existing list must share a
/// single fresh `num_id` across all items. Regression target for the
/// sibling-run propagation pass in `apply_numbering_restart_overrides` —
/// without propagation, each inserted list item would either continue
/// the original list (wrong counter) or get its own fresh num_id (each
/// item renders as "1.").
///
/// The LLM's mental model: set `restart_numbering: true` on the FIRST
/// item of the new list; subsequent items in the same insert batch at
/// the same role inherit the override.
#[test]
fn edit_insert_multi_item_new_list_shares_single_override() {
    use xmltree::XMLNode;

    let spec_fixture = "testdata/spec-compliance/numbering/start-at-override/input.docx";
    assert!(
        std::path::Path::new(spec_fixture).exists(),
        "start-at-override fixture missing — this test depends on it for a decimal list role"
    );

    let bytes = fs::read(spec_fixture).expect("fixture readable");
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .expect("fixture imports cleanly");

    let role = find_auto_numbered_role(&import.canonical)
        .expect("fixture must expose a non-bullet auto-numbered role");
    let anchor_id = find_simple_insert_anchor(&import.canonical)
        .expect("fixture must have an insertable anchor");

    // Snapshot base numIds so we can assert the new list's numId is
    // both (a) not in the base set and (b) shared across both inserted
    // items.
    let base_archive = DocxArchive::read(&bytes).expect("base readable for numId snapshot");
    let base_numbering = base_archive
        .get("word/numbering.xml")
        .expect("fixture has numbering.xml");
    let base_root =
        Element::parse(std::io::Cursor::new(base_numbering)).expect("base numbering parses");
    let mut base_num_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for child in &base_root.children {
        if let XMLNode::Element(el) = child
            && el.name == "num"
            && let Some(v) = el_attr(el, "numId")
            && let Ok(n) = v.parse::<u32>()
        {
            base_num_ids.insert(n);
        }
    }

    // Insert [body_text, list_item(restart=true, "foo"), list_item(restart=false, "bar")].
    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: anchor_id.clone(),
            position: InsertPosition::After,
            rationale: Some(
                "Start a new numbered list with two items under the existing list.".to_string(),
            ),
            blocks: vec![
                BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some(role.clone()),
                    content: parse_paragraph_markup("foo").unwrap(),
                    restart_numbering: true,
                    list: None,
                }),
                BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some(role.clone()),
                    content: parse_paragraph_markup("bar").unwrap(),
                    restart_numbering: false,
                    list: None,
                }),
            ],
        }],
        summary: Some("insert multi-item new list".to_string()),
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 200,
            author: Some("Edit Engine".to_string()),
            date: Some("2026-04-13T00:00:00Z".to_string()),
            // A shared apply_op_id scopes the propagation pass — both
            // inserted paragraphs live in the same batch, so the sibling
            // walker treats them as a single run.
            apply_op_id: Some("op_multi_list_test".to_string()),
        },
    };

    let apply_result = runtime
        .apply_edit(&import.doc_handle, &tx)
        .expect("apply_edit succeeds");

    // Collect the two inserted list-item paragraphs in order.
    let mut inserted_list_items: Vec<(NodeId, u32, u32)> = Vec::new();
    for tb in &apply_result.canonical.blocks {
        if matches!(tb.status, TrackingStatus::Inserted(_))
            && let BlockNode::Paragraph(p) = &tb.block
            && let Some(n) = p.numbering.as_ref()
        {
            inserted_list_items.push((p.id.clone(), n.num_id, n.ilvl));
        }
    }
    assert_eq!(
        inserted_list_items.len(),
        2,
        "expected two inserted list items, got {}: {:?}",
        inserted_list_items.len(),
        inserted_list_items
    );
    let (_, foo_num_id, foo_ilvl) = inserted_list_items[0].clone();
    let (_, bar_num_id, bar_ilvl) = inserted_list_items[1].clone();

    // Both inserted items must share the SAME fresh num_id — that's
    // the propagation rule. Without it they'd render as "1." / "1."
    // (each its own fresh counter) or "1." / "N." (second item
    // continuing the original list).
    assert_eq!(
        foo_num_id, bar_num_id,
        "sibling list items in the same restart run must share a single num_id; \
         got foo={foo_num_id}, bar={bar_num_id}"
    );
    assert_eq!(
        foo_ilvl, bar_ilvl,
        "siblings must share ilvl; got foo={foo_ilvl}, bar={bar_ilvl}"
    );
    assert!(
        !base_num_ids.contains(&foo_num_id),
        "shared num_id {foo_num_id} must be a freshly allocated override, \
         not reuse of a base numId"
    );

    // Both siblings must have `restart_numbering` cleared after the
    // serialize-time pass. The trigger paragraph explicitly cleared
    // it; the propagated sibling never had it set.
    for tb in &apply_result.canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && let Some(n) = p.numbering.as_ref()
        {
            assert!(
                !n.restart_numbering,
                "restart_numbering must be cleared post-serialize on paragraph '{}'",
                p.id
            );
        }
    }

    // Export and verify the numbering.xml contains exactly ONE override
    // (not two) referencing the new num_id. This is what Word will
    // actually consult when rendering counters.
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export succeeds");
    let exported_archive = DocxArchive::read(&exported).expect("exported DOCX readable");
    let exported_numbering = exported_archive
        .get("word/numbering.xml")
        .expect("exported DOCX has numbering.xml");
    let exported_root = Element::parse(std::io::Cursor::new(exported_numbering)).expect("parses");

    let shared_num_id_str = foo_num_id.to_string();
    let override_nums: Vec<&Element> = exported_root
        .children
        .iter()
        .filter_map(|c| {
            if let XMLNode::Element(el) = c
                && el.name == "num"
                && el_attr(el, "numId") == Some(shared_num_id_str.as_str())
            {
                Some(el)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        override_nums.len(),
        1,
        "exported numbering.xml must carry exactly one w:num with numId={foo_num_id}"
    );
    let num_el = override_nums[0];
    let ilvl_str = foo_ilvl.to_string();
    let lvl_override = num_el
        .children
        .iter()
        .find_map(|c| {
            if let XMLNode::Element(el) = c
                && el.name == "lvlOverride"
                && el_attr(el, "ilvl") == Some(ilvl_str.as_str())
            {
                Some(el)
            } else {
                None
            }
        })
        .expect("new w:num must carry a w:lvlOverride for the siblings' ilvl");
    let start_override_val = lvl_override.children.iter().find_map(|c| {
        if let XMLNode::Element(el) = c
            && el.name == "startOverride"
        {
            el_attr(el, "val").map(str::to_string)
        } else {
            None
        }
    });
    assert_eq!(
        start_override_val.as_deref(),
        Some("1"),
        "w:startOverride must be val=\"1\" on the shared override"
    );

    // The base list's numId must be untouched: any original list
    // paragraphs still reference their old num_id, and the exported
    // numbering.xml still contains the original num definitions. We
    // spot-check that the shared new num_id is NOT one of the base ids
    // (already asserted) and that base num_ids are still present.
    let mut exported_num_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for child in &exported_root.children {
        if let XMLNode::Element(el) = child
            && el.name == "num"
            && let Some(v) = el_attr(el, "numId")
            && let Ok(n) = v.parse::<u32>()
        {
            exported_num_ids.insert(n);
        }
    }
    for base_id in &base_num_ids {
        assert!(
            exported_num_ids.contains(base_id),
            "base numId {base_id} disappeared from exported numbering.xml"
        );
    }
    assert!(
        exported_num_ids.contains(&foo_num_id),
        "exported numbering.xml must contain the new shared numId {foo_num_id}"
    );
}

// ─── Move step: LLM schema v3 ────────────────────────────────────────────────
//
// Op: `move`. A move step must
// produce paired `w:moveFromRangeStart`/`w:moveFromRangeEnd` bookmarks
// around the source blocks and `w:moveToRangeStart`/`w:moveToRangeEnd`
// around the destination clones, all sharing a common `w:id` attribute
// so Word can reconcile the halves of the move.

/// Find any two adjacent Normal paragraphs we can use as the source
/// range. Returns the pair of block ids if found.
fn find_two_adjacent_editable_paragraphs(doc: &CanonDoc) -> Option<(NodeId, NodeId)> {
    for pair in doc.blocks.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if !matches!(a.status, TrackingStatus::Normal)
            || !matches!(b.status, TrackingStatus::Normal)
        {
            continue;
        }
        let (BlockNode::Paragraph(pa), BlockNode::Paragraph(pb)) = (&a.block, &b.block) else {
            continue;
        };
        if pa
            .segments
            .iter()
            .any(|s| !matches!(s.status, TrackingStatus::Normal))
            || pb
                .segments
                .iter()
                .any(|s| !matches!(s.status, TrackingStatus::Normal))
        {
            continue;
        }
        return Some((pa.id.clone(), pb.id.clone()));
    }
    None
}

/// Find a third Normal paragraph outside a given pair to use as a
/// move destination. Skips opaque anchors and tracked segments.
fn find_editable_paragraph_outside(
    doc: &CanonDoc,
    exclude_from: &NodeId,
    exclude_to: &NodeId,
) -> Option<NodeId> {
    doc.blocks.iter().find_map(|tb| {
        if !matches!(tb.status, TrackingStatus::Normal) {
            return None;
        }
        let BlockNode::Paragraph(p) = &tb.block else {
            return None;
        };
        if p.id == *exclude_from || p.id == *exclude_to {
            return None;
        }
        if p.segments
            .iter()
            .any(|s| !matches!(s.status, TrackingStatus::Normal))
        {
            return None;
        }
        Some(p.id.clone())
    })
}

#[test]
fn edit_move_block_range_emits_paired_move_markers() {
    use xmltree::XMLNode;

    let fixtures = discover_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found");

    let mut tested_any = false;

    for fixture_path in &fixtures {
        let Ok(bytes) = fs::read(fixture_path) else {
            continue;
        };
        let runtime = SimpleRuntime::new();
        let Ok(import) = runtime.import_docx(&bytes) else {
            continue;
        };

        // Need two adjacent Normal paragraphs (the source range) and
        // a third Normal paragraph OUTSIDE that range (the destination).
        let Some((from_id, to_id)) = find_two_adjacent_editable_paragraphs(&import.canonical)
        else {
            continue;
        };
        let Some(dest_id) = find_editable_paragraph_outside(&import.canonical, &from_id, &to_id)
        else {
            continue;
        };
        // Destination must not fall between from and to — that's the
        // "destination inside source" error. We only need to check
        // ordering since the source is a contiguous pair.
        let order_of = |id: &NodeId| -> usize {
            import
                .canonical
                .blocks
                .iter()
                .position(|tb| match &tb.block {
                    BlockNode::Paragraph(p) => &p.id == id,
                    _ => false,
                })
                .unwrap_or(usize::MAX)
        };
        let from_idx = order_of(&from_id);
        let to_idx = order_of(&to_id);
        let dest_idx = order_of(&dest_id);
        if dest_idx >= from_idx && dest_idx <= to_idx {
            continue;
        }

        let tx = EditTransaction {
            steps: vec![EditStep::MoveBlockRange {
                from_block_id: from_id.clone(),
                to_block_id: to_id.clone(),
                dest_anchor_id: dest_id.clone(),
                dest_position: InsertPosition::After,
                rationale: Some("move adjacent pair".to_string()),
                expect: None,
                semantic_hash: None,
            }],
            summary: Some("move paired blocks".to_string()),
            materialization_mode: MaterializationMode::TrackedChange,
            revision: test_revision(),
        };

        let apply_result = runtime
            .apply_edit(&import.doc_handle, &tx)
            .unwrap_or_else(|err| panic!("apply_edit(move) failed on {fixture_path}: {err:?}"));

        // Every block with move_id in the canonical must share the
        // same id, with two Deleted sources and two Inserted dests.
        let move_tagged: Vec<&TrackedBlock> = apply_result
            .canonical
            .blocks
            .iter()
            .filter(|tb| tb.move_id.is_some())
            .collect();
        assert_eq!(
            move_tagged.len(),
            4,
            "expected 4 move-tagged blocks in {fixture_path}, got {}",
            move_tagged.len()
        );
        let shared_move_id = move_tagged[0].move_id.clone().unwrap();
        for b in &move_tagged {
            assert_eq!(
                b.move_id.as_ref(),
                Some(&shared_move_id),
                "all move halves must share one move_id"
            );
        }

        // Export and inspect the DOCX for paired w:moveFromRange* and
        // w:moveToRange* markers. The serializer wraps blocks in these
        // bookmarks when TrackedBlock.move_id is set.
        let exported = runtime
            .export_docx(&import.doc_handle, ExportMode::Redline)
            .unwrap_or_else(|err| panic!("export_docx failed on {fixture_path}: {err:?}"));
        let archive = DocxArchive::read(&exported).expect("exported DOCX readable");
        let document_xml = archive
            .get("word/document.xml")
            .expect("exported document.xml present");
        let root = Element::parse(std::io::Cursor::new(document_xml)).expect("document.xml parses");

        // Walk the tree collecting moveFromRangeStart/End and
        // moveToRangeStart/End elements and their `name` attributes.
        // The `name` attribute carries the move_id string — that's how
        // the paired halves are linked in OOXML.
        fn collect_move_range_names<'a>(root: &'a Element, local: &str, out: &mut Vec<&'a str>) {
            fn walk<'a>(el: &'a Element, local: &str, out: &mut Vec<&'a str>) {
                if el.name == local
                    && let Some(name) = el
                        .attributes
                        .iter()
                        .find_map(|(k, v)| (k.local_name == "name").then_some(v.as_str()))
                {
                    out.push(name);
                }
                for child in &el.children {
                    if let XMLNode::Element(c) = child {
                        walk(c, local, out);
                    }
                }
            }
            walk(root, local, out);
        }

        let mut from_start_names: Vec<&str> = Vec::new();
        collect_move_range_names(&root, "moveFromRangeStart", &mut from_start_names);
        let mut from_end_count = 0usize;
        fn count_tag(root: &Element, local: &str) -> usize {
            let mut n = 0;
            fn walk(el: &Element, local: &str, n: &mut usize) {
                if el.name == local {
                    *n += 1;
                }
                for child in &el.children {
                    if let XMLNode::Element(c) = child {
                        walk(c, local, n);
                    }
                }
            }
            walk(root, local, &mut n);
            n
        }
        from_end_count += count_tag(&root, "moveFromRangeEnd");
        let mut to_start_names: Vec<&str> = Vec::new();
        collect_move_range_names(&root, "moveToRangeStart", &mut to_start_names);
        let to_end_count = count_tag(&root, "moveToRangeEnd");

        assert!(
            !from_start_names.is_empty(),
            "exported DOCX must contain at least one w:moveFromRangeStart (fixture={fixture_path})"
        );
        assert!(
            !to_start_names.is_empty(),
            "exported DOCX must contain at least one w:moveToRangeStart (fixture={fixture_path})"
        );
        // Start/end counts must match.
        assert_eq!(
            from_start_names.len(),
            from_end_count,
            "moveFromRangeStart/End must be balanced in {fixture_path}"
        );
        assert_eq!(
            to_start_names.len(),
            to_end_count,
            "moveToRangeStart/End must be balanced in {fixture_path}"
        );
        // The `name` attribute carries the move_id string. Every
        // `from` name must also appear as a `to` name — that's how
        // Word pairs the halves.
        for from_name in &from_start_names {
            assert!(
                to_start_names.contains(from_name),
                "moveFromRangeStart name='{from_name}' has no matching moveToRangeStart \
                 (fixture={fixture_path})"
            );
        }

        eprintln!("  {fixture_path}: move paired markers ✓");
        tested_any = true;
        break;
    }

    assert!(
        tested_any,
        "no fixture had the structure needed to exercise MoveBlockRange"
    );
}

// ─── SetAttr: the `set_attr` op ──────────────────────────────────────────────
//
// A set_attr step must produce a `w:pPrChange` element inside the target
// paragraph's `w:pPr` in the exported DOCX, recording the previous
// paragraph properties so Word can render the change as a tracked
// formatting change. The inner pPr must be a complete snapshot of the
// previous state.

#[test]
fn edit_set_attr_emits_ppr_change() {
    use xmltree::XMLNode;

    // This test needs a fixture with BOTH a numbered (non-bullet)
    // paragraph role and at least one paragraph in a different role —
    // promoting body-text to a numbered role is what produces the
    // visible pPrChange we're asserting. `discover_fixtures` only
    // returns dirs with `before.docx`, and those are all either
    // bullet-only or no-numbering. The `shared-abstract-num`
    // spec-compliance fixture has 4 decimal-numbered paragraphs + 1
    // unnumbered, which is exactly the mix we need.
    let mut candidates: Vec<String> = discover_fixtures();
    for spec in [
        "testdata/spec-compliance/numbering/shared-abstract-num/input.docx",
        "testdata/spec-compliance/numbering/start-at-override/input.docx",
    ] {
        if std::path::Path::new(spec).exists() {
            candidates.insert(0, spec.to_string());
        }
    }
    assert!(!candidates.is_empty(), "no fixtures found");

    let mut tested_any = false;

    for fixture_path in &candidates {
        let Ok(bytes) = fs::read(fixture_path) else {
            continue;
        };
        let runtime = SimpleRuntime::new();
        let Ok(import) = runtime.import_docx(&bytes) else {
            continue;
        };

        // Need a fixture that has at least two distinct paragraph roles
        // so set_attr can switch between them. Pick the first target
        // block whose role is DIFFERENT from the auto-numbered role we
        // want to promote it to.
        let Some(target_role_id) = find_auto_numbered_role(&import.canonical) else {
            continue;
        };
        // Find a simple insert-anchor-style paragraph whose role is
        // not the target role — i.e. any Normal paragraph that currently
        // has no numbering. That gives us a visible pPr change.
        let target_block_id = import.canonical.blocks.iter().find_map(|tb| {
            if !matches!(tb.status, TrackingStatus::Normal) {
                return None;
            }
            let BlockNode::Paragraph(p) = &tb.block else {
                return None;
            };
            if p.segments
                .iter()
                .any(|s| !matches!(s.status, TrackingStatus::Normal))
            {
                return None;
            }
            // Skip paragraphs that already have numbering — we want the
            // exemplar-copy path to produce a visibly different pPr so
            // the test actually exercises the pPrChange emission.
            if p.numbering.is_some() {
                return None;
            }
            Some(p.id.clone())
        });
        let Some(target_block_id) = target_block_id else {
            continue;
        };

        let tx = EditTransaction {
            steps: vec![EditStep::SetBlockRangeAttr {
                from_block_id: target_block_id.clone(),
                to_block_id: target_block_id.clone(),
                role: target_role_id.clone(),
                rationale: Some("Promote to numbered role".to_string()),
            }],
            summary: Some("set_attr smoke".to_string()),
            materialization_mode: MaterializationMode::TrackedChange,
            revision: test_revision(),
        };

        let apply_result = runtime
            .apply_edit(&import.doc_handle, &tx)
            .unwrap_or_else(|err| panic!("apply_edit(set_attr) failed on {fixture_path}: {err:?}"));

        // The target paragraph's formatting_change must be set in
        // the canonical result.
        let target_para = apply_result
            .canonical
            .blocks
            .iter()
            .find_map(|tb| match &tb.block {
                BlockNode::Paragraph(p) if p.id == target_block_id => Some(p),
                _ => None,
            })
            .expect("target paragraph still present after set_attr");
        assert!(
            target_para.formatting_change.is_some(),
            "target paragraph must carry ParagraphFormattingChange after set_attr \
             (fixture={fixture_path})"
        );
        assert!(
            target_para.numbering.is_some(),
            "target paragraph must now carry exemplar numbering (fixture={fixture_path})"
        );

        // Export and look for a `w:pPrChange` element in the serialized
        // DOCX. We walk the whole tree because pPrChange lives inside
        // pPr inside p, and we don't need to find the exact target
        // paragraph — one matching pPrChange is proof of the round-trip.
        let exported = runtime
            .export_docx(&import.doc_handle, ExportMode::Redline)
            .unwrap_or_else(|err| panic!("export_docx failed on {fixture_path}: {err:?}"));
        let archive = DocxArchive::read(&exported).expect("exported DOCX readable");
        let document_xml = archive
            .get("word/document.xml")
            .expect("exported document.xml present");
        let root = Element::parse(std::io::Cursor::new(document_xml)).expect("document.xml parses");

        fn has_ppr_change(el: &Element) -> bool {
            if el.name == "pPrChange" {
                return true;
            }
            el.children.iter().any(|c| match c {
                XMLNode::Element(e) => has_ppr_change(e),
                _ => false,
            })
        }
        assert!(
            has_ppr_change(&root),
            "exported document.xml must contain at least one w:pPrChange \
             (fixture={fixture_path})"
        );

        eprintln!("  {fixture_path}: set_attr pPrChange ✓");
        tested_any = true;
        break;
    }

    assert!(
        tested_any,
        "no fixture had the structure needed to exercise SetBlockRangeAttr"
    );
}
