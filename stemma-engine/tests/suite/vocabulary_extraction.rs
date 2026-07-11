//! Integration tests for vocabulary extraction.

use std::collections::HashSet;
use std::fs;

use stemma::vocabulary::{self, DocumentVocabulary};
use stemma::{CanonDoc, DocxRuntime, SimpleRuntime};

use crate::common;

// ---------------------------------------------------------------------------
// Invariant assertions (applied to every fixture)
// ---------------------------------------------------------------------------

/// Every paragraph in the document appears in exactly one role.
fn assert_all_paragraphs_covered(doc: &CanonDoc, vocab: &DocumentVocabulary) {
    let all_paras = common::all_paragraphs(doc);
    // Collect all paragraph NodeIds from the document.
    let all_para_ids: HashSet<String> = all_paras.iter().map(|p| p.id.to_string()).collect();

    // Collect all exemplar NodeIds from roles.
    for role in &vocab.paragraph_roles {
        // Exemplar must exist in the document (unless synthetic fallback).
        if role.count > 0 {
            assert!(
                all_para_ids.contains(&role.exemplar.to_string()),
                "exemplar {} for role '{}' not found in document",
                role.exemplar,
                role.id
            );
        }
    }

    // Total count across all roles should equal total paragraphs.
    let total_count: usize = vocab.paragraph_roles.iter().map(|r| r.count).sum();
    assert_eq!(
        total_count,
        all_paras.len(),
        "total paragraph role counts ({total_count}) != document paragraphs ({})",
        all_paras.len()
    );
}

/// Role IDs are unique within their category.
fn assert_unique_role_ids(vocab: &DocumentVocabulary) {
    let mut para_ids: HashSet<&str> = HashSet::new();
    for role in &vocab.paragraph_roles {
        assert!(
            para_ids.insert(&role.id),
            "duplicate paragraph role id: {}",
            role.id
        );
    }

    let mut inline_ids: HashSet<&str> = HashSet::new();
    for role in &vocab.inline_roles {
        assert!(
            inline_ids.insert(&role.id),
            "duplicate inline role id: {}",
            role.id
        );
    }

    let mut table_ids: HashSet<&str> = HashSet::new();
    for role in &vocab.table_roles {
        assert!(
            table_ids.insert(&role.id),
            "duplicate table role id: {}",
            role.id
        );
    }
}

/// Universal invariants on every vocabulary.
fn assert_invariants(doc: &CanonDoc, vocab: &DocumentVocabulary) {
    // inline_marks is always 6 universal marks.
    assert_eq!(
        vocab.inline_marks,
        &[
            "bold",
            "italic",
            "underline",
            "strike",
            "subscript",
            "superscript"
        ],
    );

    // paragraph_roles is non-empty (body_text fallback).
    assert!(
        !vocab.paragraph_roles.is_empty(),
        "paragraph_roles must not be empty"
    );

    // Roles with has_numbering: true have numbering_source.is_some().
    for role in &vocab.paragraph_roles {
        if role.has_numbering {
            assert!(
                role.numbering_source.is_some(),
                "role '{}' has numbering but no numbering_source",
                role.id
            );
        }
    }

    // Description strings are non-empty.
    for role in &vocab.paragraph_roles {
        assert!(
            !role.description.is_empty(),
            "empty description for role '{}'",
            role.id
        );
    }
    for role in &vocab.inline_roles {
        assert!(
            !role.description.is_empty(),
            "empty description for inline role '{}'",
            role.id
        );
    }
    for role in &vocab.table_roles {
        assert!(
            !role.description.is_empty(),
            "empty description for table role '{}'",
            role.id
        );
    }

    assert_all_paragraphs_covered(doc, vocab);
    assert_unique_role_ids(vocab);
}

// ---------------------------------------------------------------------------
// Helper: load a fixture from testdata/{name}/before.docx
// ---------------------------------------------------------------------------

fn load_testdata(name: &str) -> (SimpleRuntime, CanonDoc) {
    let path = format!("testdata/{name}/before.docx");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {path}: {e:?}"));
    let view = runtime.view(&import.doc_handle).expect("view");
    (runtime, std::sync::Arc::unwrap_or_clone(view.canonical))
}

fn load_sample(name: &str) -> (SimpleRuntime, CanonDoc) {
    let path = common::samples_dir().join(format!("{name}/before.docx"));
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {}: {e:?}", path.display()));
    let view = runtime.view(&import.doc_handle).expect("view");
    (runtime, std::sync::Arc::unwrap_or_clone(view.canonical))
}

// ---------------------------------------------------------------------------
// Tests per fixture
// ---------------------------------------------------------------------------

#[test]
fn safe_valcap_contract() {
    let (_rt, doc) = load_testdata("safe-valcap-vs-discount");
    let vocab = vocabulary::extract_vocabulary(&doc);
    assert_invariants(&doc, &vocab);

    // SAFE contract should have title, headings, numbered items, body text.
    let role_ids: Vec<&str> = vocab
        .paragraph_roles
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert!(
        vocab.paragraph_roles.len() >= 2,
        "expected at least 2 paragraph roles, got {} — roles: {:?}",
        vocab.paragraph_roles.len(),
        role_ids
    );
}

#[test]
fn simple_text_few_roles() {
    let (_rt, doc) = load_testdata("simple-text");
    let vocab = vocabulary::extract_vocabulary(&doc);
    assert_invariants(&doc, &vocab);

    // Minimal doc: should have few roles.
    assert!(
        vocab.paragraph_roles.len() <= 10,
        "expected few roles for simple doc, got {}",
        vocab.paragraph_roles.len()
    );
}

#[test]
fn twenty_paragraphs_consolidation() {
    let (_rt, doc) = load_testdata("twenty-paragraphs");
    let vocab = vocabulary::extract_vocabulary(&doc);
    assert_invariants(&doc, &vocab);

    let all_paras = common::all_paragraphs(&doc);
    // Grouping should consolidate: fewer roles than paragraphs.
    assert!(
        vocab.paragraph_roles.len() < all_paras.len(),
        "expected fewer roles ({}) than paragraphs ({})",
        vocab.paragraph_roles.len(),
        all_paras.len()
    );
}

#[test]
fn table_changes_has_table_roles() {
    let (_rt, doc) = load_testdata("table-changes");
    let vocab = vocabulary::extract_vocabulary(&doc);
    assert_invariants(&doc, &vocab);

    // Should have at least one table role.
    assert!(
        !vocab.table_roles.is_empty(),
        "expected table roles for table-changes fixture"
    );
}

#[test]
#[ignore = "requires private corpus (edgar-saas, real third-party); run via just nightly"]
fn edgar_saas_no_styles() {
    let (_rt, doc) = load_sample("edgar-saas");
    let vocab = vocabulary::extract_vocabulary(&doc);
    assert_invariants(&doc, &vocab);

    // All style_id: None — pure pattern clustering should still produce roles.
    assert!(
        vocab.paragraph_roles.len() >= 2,
        "expected at least 2 roles from pattern clustering, got {}",
        vocab.paragraph_roles.len()
    );
}

#[test]
#[ignore = "requires private corpus (humira-epar, real third-party); run via just nightly"]
fn humira_epar_large_doc() {
    let (_rt, doc) = load_sample("humira-epar");
    let vocab = vocabulary::extract_vocabulary(&doc);
    assert_invariants(&doc, &vocab);

    // Large regulatory doc: many styles.
    assert!(
        vocab.paragraph_roles.len() >= 3,
        "expected many roles for humira-epar, got {}",
        vocab.paragraph_roles.len()
    );
}
