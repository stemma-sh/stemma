//! Shared test helpers for integration tests.

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::process::Command;

use quick_xml::Reader;
use quick_xml::events::Event;
use stemma::{BlockNode, CanonDoc, DocxRuntime, InlineNode, SimpleRuntime, TrackedBlock};

// ─── Element census (shared with corpus_triage / element_fidelity) ─────────

/// Read a named entry from a DOCX (ZIP) byte buffer as a UTF-8 string.
/// Returns `None` when the archive can't be opened or the entry is absent.
#[allow(dead_code)]
pub fn read_zip_entry(docx_bytes: &[u8], name: &str) -> Option<String> {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = zip::ZipArchive::new(cursor).ok()?;
    let mut file = zip.by_name(name).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// Census every element in an XML string by its (possibly prefixed) local name,
/// e.g. `w:numId`, `w:gridSpan`, `w14:paraId`. Counts both non-empty
/// (`<w:p>`) and empty (`<w:gridSpan/>`) element opens.
///
/// This is the same census quick-xml drives in `corpus_triage::build_element_census`;
/// it is factored here so the element-fidelity gate and triage share one definition.
#[allow(dead_code)]
pub fn build_element_census(xml: &str) -> HashMap<String, usize> {
    let mut census: HashMap<String, usize> = HashMap::new();
    if xml.is_empty() {
        return census;
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                *census.entry(name).or_default() += 1;
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    census
}

/// Resolve the corpus root directory.
///
/// DOCX corpus files (samples, stress) live in the main checkout and are NOT
/// committed to git.  Worktrees reach them via `STEMMA_CORPUS_ROOT`.
/// Falls back to the repo root relative to CARGO_MANIFEST_DIR (works in the
/// main checkout where the files are present on disk).
#[allow(dead_code)]
pub fn corpus_root() -> PathBuf {
    if let Ok(root) = std::env::var("STEMMA_CORPUS_ROOT") {
        return PathBuf::from(root);
    }
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        && output.status.success()
    {
        let git_common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !git_common_dir.is_empty() {
            let common_dir = PathBuf::from(git_common_dir);
            if let Some(root) = common_dir.parent() {
                return root.to_path_buf();
            }
        }
    }
    // Default: repo root relative to stemma-engine/
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Path to `backend/samples/` in the corpus.
#[allow(dead_code)]
pub fn samples_dir() -> PathBuf {
    corpus_root().join("backend").join("samples")
}

/// Path to `backend-xml/stress/` in the corpus.
#[allow(dead_code)]
pub fn stress_dir() -> PathBuf {
    corpus_root().join("backend-xml").join("stress")
}

/// Import a fixture DOCX from `{fixtures}/{name}/input.docx`, returning the
/// runtime and the canonical document (via `view()`).
#[allow(dead_code)]
pub fn import_fixture(fixtures: &str, name: &str) -> (SimpleRuntime, CanonDoc) {
    let path = format!("{fixtures}/{name}/input.docx");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {path}: {e:?}"));
    let view = runtime.view(&import.doc_handle).expect("view");
    (runtime, std::sync::Arc::unwrap_or_clone(view.canonical))
}

/// Collect all paragraphs from a canonical document, recursing into tables.
#[allow(dead_code)]
pub fn all_paragraphs(doc: &CanonDoc) -> Vec<&stemma::ParagraphNode> {
    let mut paras = Vec::new();
    collect_paragraphs_tracked(&doc.blocks, &mut paras);
    paras
}

/// Collect paragraphs from tracked blocks (top-level document blocks).
#[allow(dead_code)]
pub fn collect_paragraphs_tracked<'a>(
    blocks: &'a [TrackedBlock],
    out: &mut Vec<&'a stemma::ParagraphNode>,
) {
    for tracked in blocks {
        match &tracked.block {
            BlockNode::Paragraph(p) => out.push(p),
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_paragraphs_bare(&cell.blocks, out);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

/// Collect paragraphs from bare (non-tracked) blocks (e.g. table cell contents).
#[allow(dead_code)]
pub fn collect_paragraphs_bare<'a>(
    blocks: &'a [BlockNode],
    out: &mut Vec<&'a stemma::ParagraphNode>,
) {
    for block in blocks {
        match block {
            BlockNode::Paragraph(p) => out.push(p),
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_paragraphs_bare(&cell.blocks, out);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

/// Extract the text content of a paragraph, including hard breaks as newlines.
#[allow(dead_code)]
pub fn paragraph_text(p: &stemma::ParagraphNode) -> String {
    let inlines = p.all_inlines_owned();
    let mut out = String::new();
    for inline in &inlines {
        match inline {
            InlineNode::Text(t) => out.push_str(&t.text),
            InlineNode::HardBreak(_) => out.push('\n'),
            _ => {}
        }
    }
    out
}
