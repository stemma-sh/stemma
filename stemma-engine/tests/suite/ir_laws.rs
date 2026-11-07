//! The engine's IR laws, checked over every fixture document in `testdata/`.
//!
//! Stemma reads a DOCX into one canonical model, transforms it, and writes it
//! back. These laws state what that requires:
//!
//! - **L1 One spelling:** saving and reopening a document yields the same model.
//! - **L2a Projection commutes with save:** an Accept All or Reject All reading
//!   is the same model before and after it is saved and reopened.
//! - **L2b Resolution of nothing changes nothing:** Accept All of a document
//!   with no revisions is the document, apart from Word's own full-resolution
//!   cleanup: Word removes proofing-error markers (`w:proofErr`) on Accept All,
//!   which also rejoins the text they split. Both sides are compared without
//!   proofing markers and run boundaries, which are not content here.
//! - **L3 Identity:** comparing a document with itself finds no change.
//!
//! "The same model" is `roundtrip_compare::compare_canon_docs`, which ignores
//! addressing (node ids, hashes) but nothing a reader can observe. Documents
//! the engine refuses to import are outside these laws: the refusal is their
//! contract.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use stemma::ExportOptions;
use stemma::api::{Document, Resolution};
use stemma::domain::{BlockNode, CanonDoc, DecorationType, InlineNode, TrackedBlock};
use stemma::roundtrip_compare::{Difference, compare_canon_docs};

fn fixture_documents() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read fixture directory") {
            let path = entry.expect("fixture entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "docx")
            {
                out.push(path);
            }
        }
    }
    let mut documents = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata"),
        &mut documents,
    );
    documents.sort();
    documents
}

fn reopen(document: &Document) -> Result<Document, String> {
    let bytes = document
        .serialize(&ExportOptions::default())
        .map_err(|err| format!("serialize: {err:?}"))?;
    Document::parse(&bytes).map_err(|err| format!("reopen: {err:?}"))
}

fn summarize(diffs: &[Difference]) -> String {
    let shown: Vec<String> = diffs.iter().take(3).map(ToString::to_string).collect();
    format!("{} difference(s): {}", diffs.len(), shown.join("; "))
}

/// Remove proofing-error markers and run boundaries, symmetrically: adjacent
/// text nodes that look the same are merged, and editing-session run
/// attributes are dropped. What remains is the content a reader observes.
fn without_proofing_and_run_boundaries(document: &CanonDoc) -> CanonDoc {
    fn inlines(list: &mut Vec<InlineNode>) {
        let mut merged: Vec<InlineNode> = Vec::with_capacity(list.len());
        for inline in list.drain(..) {
            match inline {
                InlineNode::Decoration(decoration)
                    if decoration.kind == DecorationType::ProofError => {}
                InlineNode::Text(mut text) => {
                    text.source_run_attrs.clear();
                    if let Some(InlineNode::Text(previous)) = merged.last_mut()
                        && previous.text_role == text.text_role
                        && previous.formatting_change == text.formatting_change
                        && previous.marks == text.marks
                        && previous.style_props == text.style_props
                        && previous.rpr_authored == text.rpr_authored
                    {
                        previous.text.push_str(&text.text);
                    } else {
                        merged.push(InlineNode::Text(text));
                    }
                }
                other => merged.push(other),
            }
        }
        *list = merged;
    }
    fn block(node: &mut BlockNode) {
        match node {
            BlockNode::Paragraph(paragraph) => {
                for segment in &mut paragraph.segments {
                    inlines(&mut segment.inlines);
                }
            }
            BlockNode::Table(table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        cell.blocks.iter_mut().for_each(block);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    fn story(blocks: &mut [TrackedBlock]) {
        blocks
            .iter_mut()
            .for_each(|tracked| block(&mut tracked.block));
    }
    let mut normalized = document.clone();
    story(&mut normalized.blocks);
    normalized
        .headers
        .iter_mut()
        .for_each(|s| story(&mut s.blocks));
    normalized
        .footers
        .iter_mut()
        .for_each(|s| story(&mut s.blocks));
    normalized
        .footnotes
        .iter_mut()
        .for_each(|s| story(&mut s.blocks));
    normalized
        .endnotes
        .iter_mut()
        .for_each(|s| story(&mut s.blocks));
    normalized
        .comments
        .iter_mut()
        .for_each(|s| story(&mut s.blocks));
    normalized
}

/// Every law violated by one document, as `"<law>: <detail>"`.
fn violations(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).expect("read fixture");
    let Ok(document) = Document::parse(&bytes) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let original = &document.snapshot().canonical;

    match reopen(&document) {
        Ok(reopened) => {
            let diffs = compare_canon_docs(original, &reopened.snapshot().canonical);
            if !diffs.is_empty() {
                found.push(format!("L1: {}", summarize(&diffs)));
            }
        }
        Err(err) => found.push(format!("L1: {err}")),
    }

    for (label, resolution) in [
        ("Accept All", Resolution::AcceptAll),
        ("Reject All", Resolution::RejectAll),
    ] {
        let accept_all = matches!(resolution, Resolution::AcceptAll);
        let projection = match document.project(resolution) {
            Ok(projection) => projection,
            Err(err) => {
                found.push(format!("L2a {label}: projection refused: {err:?}"));
                continue;
            }
        };
        match reopen(&projection) {
            Ok(reopened) => {
                let diffs = compare_canon_docs(
                    &projection.snapshot().canonical,
                    &reopened.snapshot().canonical,
                );
                if !diffs.is_empty() {
                    found.push(format!("L2a {label}: {}", summarize(&diffs)));
                }
            }
            Err(err) => found.push(format!("L2a {label}: {err}")),
        }
        if accept_all && document.revisions().is_empty() {
            let diffs = compare_canon_docs(
                &without_proofing_and_run_boundaries(original),
                &without_proofing_and_run_boundaries(&projection.snapshot().canonical),
            );
            if !diffs.is_empty() {
                found.push(format!("L2b: {}", summarize(&diffs)));
            }
        }
    }

    match stemma_diff::diff_detailed(&document, &document) {
        Ok(result) if result.semantic_change_count == 0 => {}
        Ok(result) => found.push(format!("L3: {} change(s)", result.semantic_change_count)),
        Err(err) => found.push(format!("L3: comparison failed: {err:?}")),
    }
    found
}

#[test]
fn every_fixture_document_obeys_the_ir_laws() {
    let documents = fixture_documents();
    assert!(
        documents.len() > 600,
        "expected the engine fixtures, found {}",
        documents.len()
    );

    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(path) = documents.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let name = path
                        .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata"))
                        .expect("fixture under testdata")
                        .display()
                        .to_string();
                    for violation in violations(path) {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("{name}: {violation}"));
                    }
                }
            });
        }
    });

    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    assert!(
        failures.is_empty(),
        "{} IR-law violation(s) across {} documents:\n{}",
        failures.len(),
        documents.len(),
        failures.join("\n")
    );
}
