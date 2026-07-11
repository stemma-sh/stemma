//! Cross-path accept-all / reject-all PROJECTION equivalence.
//!
//! Stemma has TWO implementations of tracked-change accept-all/reject-all:
//!
//!   - MODEL path: `stemma::accept_all` / `stemma::reject_all`
//!     (tracked_model.rs, operating on the parsed `CanonDoc` IR) — also the
//!     engine behind `Document::read_accepted()` / `read_rejected()`.
//!   - BYTE path: `stemma::normalize::normalize_docx` (accept) and
//!     `stemma::normalize::reject_all_docx` (reject), operating on the raw
//!     XML of a `DocxArchive`.
//!
//! THE INVARIANT: for any document,
//!
//!     text-projection(model accept_all of the parsed doc)
//!       == text-projection(reimport of byte-path normalize_docx output)
//!
//! and the same for reject. The two paths disagreed for YEARS on
//! paragraph-mark joins (the byte path dropped the pPr/rPr ins/del markers
//! but never merged the paragraphs) because no test ever compared them
//! against each other. This file is that comparison, run daily over
//! synthetic in-repo fixtures, plus an `#[ignore]`d sweep over the corpus.
//!
//! THE PROJECTION (the "text channel", mirroring
//! the consuming application's verify_representative binary):
//!   - per-paragraph list, recursing into table cells (and nested tables);
//!   - opaque inlines render as U+FFFC (NOT dropped — a known historical
//!     divergence, opaque-only paragraph joins, was invisible in a
//!     text-only comparison);
//!   - hyperlink opaques render their visible run text (so tracked changes
//!     INSIDE hyperlink display runs are comparable, not masked by U+FFFC);
//!   - `literal_prefix` is prepended (tab-separated), caps formatting
//!     uppercases, hard breaks are newlines;
//!   - whitespace is normalized within a paragraph and empty paragraphs are
//!     dropped (mirroring the verifier) — but U+FFFC is kept.
//!
//! The projection FAILS LOUD if any tracked state survives the resolution
//! (segment/block/row/cell/mark status, hyperlink run status, or a
//! `*PrChange` record): a leftover means one path didn't actually resolve,
//! which is itself a cross-path bug, not something to project around.

use crate::common;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use rayon::prelude::*;
use stemma::docx::DocxArchive;
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::{
    BlockNode, CanonDoc, DocxRuntime, InlineNode, MarkValue, OpaqueKind, ParagraphNode,
    SimpleRuntime, TrackedBlock, TrackingStatus, accept_all, reject_all_with_styles,
};
use zip::write::FileOptions;

// ═══════════════════════════════════════════════════════════════════════════
// Minimal in-memory DOCX builder (same shape as spec_stacked_revisions.rs)
// ═══════════════════════════════════════════════════════════════════════════

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
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

// ═══════════════════════════════════════════════════════════════════════════
// The text projection (verify_representative.rs conventions)
// ═══════════════════════════════════════════════════════════════════════════

/// Panic helper: a resolution (accept-all or reject-all) must leave ZERO
/// tracked state behind. Anything else means one path didn't actually
/// resolve — fail loud with the case + path label, never project around it.
fn assert_resolved(condition: bool, label: &str, what: &str) {
    assert!(
        condition,
        "[{label}] resolution left tracked state behind: {what}"
    );
}

/// Render one paragraph through the text channel:
/// literal_prefix + '\t', text (caps uppercased), '\n' for hard breaks,
/// U+FFFC for opaque inlines (hyperlinks render their run text instead so
/// tracked changes inside display runs stay comparable), zero-width
/// decorations/comment markers skipped.
fn render_paragraph(p: &ParagraphNode, label: &str) -> String {
    assert_resolved(
        matches!(p.para_mark_status, None | Some(TrackingStatus::Normal)),
        label,
        "paragraph mark still tracked",
    );
    assert_resolved(
        p.formatting_change.is_none(),
        label,
        "pPrChange record survived",
    );

    let mut out = String::new();
    if let Some(prefix) = &p.literal_prefix {
        out.push_str(prefix);
        out.push('\t');
    }
    for seg in &p.segments {
        assert_resolved(
            matches!(seg.status, TrackingStatus::Normal),
            label,
            "segment still tracked",
        );
        for inline in &seg.inlines {
            match inline {
                InlineNode::Text(t) => {
                    assert_resolved(
                        t.formatting_change.is_none(),
                        label,
                        "rPrChange record survived",
                    );
                    if t.style_props.caps == MarkValue::On {
                        out.push_str(&t.text.to_uppercase());
                    } else {
                        out.push_str(&t.text);
                    }
                }
                InlineNode::HardBreak(_) => out.push('\n'),
                InlineNode::OpaqueInline(o) => {
                    if let OpaqueKind::Hyperlink(data) = &o.kind {
                        // Hyperlink display text is comparable content, not an
                        // opaque blob: render the runs so a tracked change
                        // inside the display text shows up in the channel.
                        // `data.text` is the documented fallback for
                        // synthetically built hyperlinks with no parsed runs.
                        if data.runs.is_empty() {
                            out.push_str(&data.text);
                        } else {
                            for run in &data.runs {
                                assert_resolved(
                                    matches!(run.status, TrackingStatus::Normal),
                                    label,
                                    "hyperlink run still tracked",
                                );
                                out.push_str(&run.text);
                            }
                        }
                    } else {
                        out.push('\u{FFFC}');
                    }
                }
                InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. } => {}
            }
        }
    }
    out
}

fn collect_tracked_blocks(blocks: &[TrackedBlock], label: &str, out: &mut Vec<String>) {
    for tb in blocks {
        assert_resolved(
            matches!(tb.status, TrackingStatus::Normal),
            label,
            "block still tracked",
        );
        collect_block(&tb.block, label, out);
    }
}

fn collect_block(block: &BlockNode, label: &str, out: &mut Vec<String>) {
    match block {
        BlockNode::Paragraph(p) => out.push(render_paragraph(p, label)),
        BlockNode::Table(t) => {
            assert_resolved(t.formatting_change.is_none(), label, "tblPrChange survived");
            for row in &t.rows {
                assert_resolved(
                    matches!(row.tracking_status, None | Some(TrackingStatus::Normal)),
                    label,
                    "table row still tracked",
                );
                assert_resolved(
                    row.formatting_change.is_none(),
                    label,
                    "trPrChange survived",
                );
                for cell in &row.cells {
                    assert_resolved(
                        matches!(cell.tracking_status, None | Some(TrackingStatus::Normal)),
                        label,
                        "table cell still tracked",
                    );
                    assert_resolved(
                        cell.formatting_change.is_none(),
                        label,
                        "tcPrChange survived",
                    );
                    for nested in &cell.blocks {
                        collect_block(nested, label, out);
                    }
                }
            }
        }
        // A block-level opaque (sdt, block field, ...) occupies a slot in the
        // flow; keep it visible as a U+FFFC paragraph so a path that DROPS the
        // block (or replaces it with real content) diverges loudly. Its inner
        // bytes are not compared (the model path intentionally preserves
        // opaque raw XML verbatim, revisions included).
        BlockNode::OpaqueBlock(_) => out.push("\u{FFFC}".to_string()),
    }
}

/// The full projection: per-paragraph channel texts, whitespace-normalized
/// within each paragraph, empty paragraphs dropped (U+FFFC is not whitespace,
/// so opaque-only paragraphs survive — that's the point).
fn project(doc: &CanonDoc, label: &str) -> Vec<String> {
    let mut raw = Vec::new();
    collect_tracked_blocks(&doc.blocks, label, &mut raw);
    raw.iter()
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// The two paths
// ═══════════════════════════════════════════════════════════════════════════

fn import_canonical(bytes: &[u8], label: &str) -> CanonDoc {
    let runtime = SimpleRuntime::new();
    std::sync::Arc::unwrap_or_clone(
        runtime
            .import_docx(bytes)
            .unwrap_or_else(|e| panic!("[{label}] import: {e:?}"))
            .canonical,
    )
}

/// MODEL path: parse → `accept_all` / `reject_all` on the IR → project.
fn model_projection(canon: &CanonDoc, accept: bool, label: &str) -> Vec<String> {
    let mut doc = canon.clone();
    if accept {
        accept_all(&mut doc);
    } else {
        reject_all_with_styles(&mut doc, None);
    }
    project(&doc, label)
}

/// BYTE path: `normalize_docx` / `reject_all_docx` on the raw XML → write →
/// REIMPORT → project. The reimport is the honest reading of what the byte
/// path actually produced on disk.
fn byte_projection(bytes: &[u8], accept: bool, label: &str) -> Vec<String> {
    let archive =
        DocxArchive::read(bytes).unwrap_or_else(|e| panic!("[{label}] DocxArchive::read: {e:?}"));
    let (resolved, _result) = if accept {
        normalize_docx(&archive).unwrap_or_else(|e| panic!("[{label}] normalize_docx: {e:?}"))
    } else {
        reject_all_docx(&archive).unwrap_or_else(|e| panic!("[{label}] reject_all_docx: {e:?}"))
    };
    let out_bytes = resolved
        .write()
        .unwrap_or_else(|e| panic!("[{label}] write resolved archive: {e:?}"));
    let canon = import_canonical(
        &out_bytes,
        &format!("{label} (reimport of byte-path output)"),
    );
    project(&canon, label)
}

/// Run ONE fixture through both paths, in both directions, and pin the
/// expected projections from the domain rules (so both paths sharing a bug
/// can't pass silently).
fn check_cross_path(name: &str, bytes: &[u8], expected_accept: &[&str], expected_reject: &[&str]) {
    let canon = import_canonical(bytes, name);

    for (direction, expected) in [("accept", expected_accept), ("reject", expected_reject)] {
        let accept = direction == "accept";
        let label_model = format!("{name}/{direction}/model");
        let label_byte = format!("{name}/{direction}/byte");
        let model = model_projection(&canon, accept, &label_model);
        let byte = byte_projection(bytes, accept, &label_byte);

        assert_eq!(
            model, byte,
            "[{name}] {direction}-all CROSS-PATH DIVERGENCE:\n  \
             model path (accept_all/reject_all on IR): {model:#?}\n  \
             byte  path (normalize_docx/reject_all_docx + reimport): {byte:#?}"
        );
        assert_eq!(
            model,
            expected.to_vec(),
            "[{name}] {direction}-all: both paths agree but NOT on the \
             domain-expected projection (shared bug?)"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Fixture attribute helpers
// ═══════════════════════════════════════════════════════════════════════════

const INS_A: &str = r#"w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z""#;
const DEL_B: &str = r#"w:id="2" w:author="AuthorB" w:date="2026-02-01T00:00:00Z""#;

// ═══════════════════════════════════════════════════════════════════════════
// Daily fixtures — one test per family for attributable failures
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn plain_inline_insertion() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Start </w:t></w:r><w:ins {INS_A}><w:r><w:t xml:space="preserve">added </w:t></w:r></w:ins><w:r><w:t>end.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "plain-inline-ins",
        &make_docx_with_body(&body),
        &["Start added end."],
        &["Start end."],
    );
}

#[test]
fn plain_inline_deletion() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Start </w:t></w:r><w:del {DEL_B}><w:r><w:delText xml:space="preserve">cut </w:delText></w:r></w:del><w:r><w:t>end.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "plain-inline-del",
        &make_docx_with_body(&body),
        &["Start end."],
        &["Start cut end."],
    );
}

#[test]
fn deleted_paragraph_mark_joins_on_accept() {
    // §17.13.5.15: accepting a deleted paragraph mark removes the break —
    // the paragraph's content merges into the FOLLOWING paragraph. Reject
    // keeps the break. This is the exact shape the byte path got wrong for
    // years (it dropped the marker but never joined).
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:del {DEL_B}/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">First part</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> second part.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "deleted-para-mark",
        &make_docx_with_body(&body),
        &["First part second part."],
        &["First part", "second part."],
    );
}

#[test]
fn chained_deleted_paragraph_marks_join_transitively() {
    // Two consecutive deleted marks: accept joins all three paragraphs.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:del {DEL_B}/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">Alpha</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:del w:id="3" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve"> beta</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> gamma.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "chained-deleted-para-marks",
        &make_docx_with_body(&body),
        &["Alpha beta gamma."],
        &["Alpha", "beta", "gamma."],
    );
}

#[test]
fn inserted_paragraph_mark_joins_on_reject() {
    // §17.13.5.19: an inserted paragraph mark. Accept keeps the new break;
    // reject un-proposes it and the paragraphs join.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins {INS_A}/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">Alpha</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> beta.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "inserted-para-mark",
        &make_docx_with_body(&body),
        &["Alpha", "beta."],
        &["Alpha beta."],
    );
}

#[test]
fn stacked_paragraph_mark_joins_in_both_directions() {
    // Both markers in pPr/rPr (inserted by one pending revision, deleted by
    // another): the break survives NEITHER full resolution (origin rules).
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins {INS_A}/><w:del {DEL_B}/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">First part</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> second part.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "stacked-para-mark",
        &make_docx_with_body(&body),
        &["First part second part."],
        &["First part second part."],
    );
}

#[test]
fn deleted_literal_prefix_label() {
    // The prefix-hoist defect shape: a Word redline deletes the literal
    // enumeration label "1." (and its tab) at the start of the paragraph.
    // The label is TRACKED text — accepting the deletion removes it, reject
    // restores it. Import must NOT hoist it into the untracked
    // `literal_prefix` field, or the model path can never resolve it.
    let body = format!(
        r#"<w:p><w:del {DEL_B}><w:r><w:delText>1.</w:delText></w:r><w:r><w:tab/></w:r></w:del><w:r><w:t>Body clause text.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "deleted-literal-prefix",
        &make_docx_with_body(&body),
        &["Body clause text."],
        &["1. Body clause text."],
    );
}

#[test]
fn inserted_literal_prefix_label() {
    // Inserted label "(a)" + tab: accept keeps it, reject removes it — and
    // must not leave a stale untracked literal_prefix behind.
    let body = format!(
        r#"<w:p><w:ins {INS_A}><w:r><w:t>(a)</w:t></w:r><w:r><w:tab/></w:r></w:ins><w:r><w:t>Body clause text.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "inserted-literal-prefix",
        &make_docx_with_body(&body),
        &["(a) Body clause text."],
        &["Body clause text."],
    );
}

#[test]
fn label_consumed_from_partially_tracked_run() {
    // The label is the FRONT of a single inserted run "(a) New clause
    // text.": the hoist would consume tracked characters from a run that
    // otherwise survives, so the refusal must cover partial consumption,
    // not just wholly-removed inlines. Reject drops the entire insertion —
    // no "(a)" residue may remain.
    let body = format!(
        r#"<w:p><w:ins {INS_A}><w:r><w:t xml:space="preserve">(a) New clause text.</w:t></w:r></w:ins></w:p><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "partially-tracked-prefix-run",
        &make_docx_with_body(&body),
        &["(a) New clause text.", "Tail."],
        &["Tail."],
    );
}

#[test]
fn hoisted_prefix_survives_inserted_mark_join_on_reject() {
    // A paragraph with an UNTRACKED literal label "(e)" but an INSERTED
    // paragraph mark (a tracked split). Rejecting the mark joins the
    // paragraph into the following one — the donor's hoisted literal_prefix
    // is real text and must survive the join at the front of the merged
    // content (the byte path keeps its literal runs).
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins {INS_A}/></w:rPr></w:pPr><w:r><w:t>(e)</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>First part</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> second part.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "hoisted-prefix-inserted-mark-join",
        &make_docx_with_body(&body),
        &["(e) First part", "second part."],
        &["(e) First part second part."],
    );
}

#[test]
fn hoisted_prefix_survives_deleted_mark_join_on_accept() {
    // Same shape with a DELETED paragraph mark: accepting joins, and the
    // donor's hoisted label must lead the merged paragraph.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:del {DEL_B}/></w:rPr></w:pPr><w:r><w:t>(a)</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>First part</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> second part.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "hoisted-prefix-deleted-mark-join",
        &make_docx_with_body(&body),
        &["(a) First part second part."],
        &["(a) First part", "second part."],
    );
}

#[test]
fn both_paragraphs_hoisted_prefixes_join() {
    // BOTH paragraphs carry hoisted labels and the first mark is deleted:
    // on accept the donor's "(a)" leads the merged paragraph and the
    // target's "(b)" becomes plain text mid-paragraph (that's where its
    // literal bytes sit in the merged XML).
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:del {DEL_B}/></w:rPr></w:pPr><w:r><w:t>(a)</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t xml:space="preserve">Alpha </w:t></w:r></w:p><w:p><w:r><w:t>(b)</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>beta.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "both-prefixes-join",
        &make_docx_with_body(&body),
        &["(a) Alpha (b) beta."],
        &["(a) Alpha", "(b) beta."],
    );
}

#[test]
fn untracked_literal_prefix_still_hoists() {
    // Control for the tracked-prefix refusal: a plain untracked label still
    // hoists at import, and both resolutions keep it verbatim.
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">(a) Plain clause text.</w:t></w:r></w:p><w:p><w:ins {INS_A}><w:r><w:t xml:space="preserve">added </w:t></w:r></w:ins><w:r><w:t>tail.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "untracked-literal-prefix",
        &make_docx_with_body(&body),
        &["(a) Plain clause text.", "added tail."],
        &["(a) Plain clause text.", "tail."],
    );
}

// A block equation (m:oMathPara, §22.1.2.78) with its namespace declared
// inline. The importer treats it as an opaque inline (OpaqueKind::OmmlBlock).
const OMATH_PARA: &str = r#"<m:oMathPara xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:oMath><m:r><m:t>x=1</m:t></m:r></m:oMath></m:oMathPara>"#;

#[test]
fn inserted_block_math() {
    // The block-math-in-revision defect shape: m:oMathPara wrapped in w:ins.
    // The equation is tracked content — accept keeps it (as an opaque
    // U+FFFC in the channel), reject removes it. Import used to drop the
    // math entirely because widget recognition only looked at direct
    // children of w:p.
    let body = format!(
        r#"<w:p><w:r><w:t>Before.</w:t></w:r></w:p><w:p><w:ins {INS_A}>{OMATH_PARA}</w:ins></w:p><w:p><w:r><w:t>After.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "inserted-block-math",
        &make_docx_with_body(&body),
        &["Before.", "\u{FFFC}", "After."],
        &["Before.", "After."],
    );
}

#[test]
fn deleted_block_math() {
    let body = format!(
        r#"<w:p><w:r><w:t>Before.</w:t></w:r></w:p><w:p><w:del {DEL_B}>{OMATH_PARA}</w:del></w:p><w:p><w:r><w:t>After.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "deleted-block-math",
        &make_docx_with_body(&body),
        &["Before.", "After."],
        &["Before.", "\u{FFFC}", "After."],
    );
}

// A real DrawingML inline (namespaces declared inline so the fixture stays
// self-contained). The importer treats `w:drawing` as an opaque inline.
const DRAWING_RUN: &str = r#"<w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" distT="0" distB="0" distL="0" distR="0"><wp:extent cx="304800" cy="304800"/><wp:docPr id="1" name="Picture 1"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"/></a:graphic></wp:inline></w:drawing></w:r>"#;

#[test]
fn opaque_only_paragraph_with_deleted_mark() {
    // THE historically invisible case: a paragraph whose only content is an
    // opaque inline (a drawing), with a DELETED paragraph mark. Accepting
    // the mark deletion joins the drawing into the following paragraph. A
    // text-only projection saw an "empty" paragraph on both sides and the
    // divergence hid; the U+FFFC channel makes the join observable.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:del {DEL_B}/></w:rPr></w:pPr>{DRAWING_RUN}</w:p><w:p><w:r><w:t xml:space="preserve">After the image.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "opaque-only-deleted-mark",
        &make_docx_with_body(&body),
        &["\u{FFFC}After the image."],
        &["\u{FFFC}", "After the image."],
    );
}

#[test]
fn deleted_table_row() {
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4675"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row one.</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:del {DEL_B}/></w:trPr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:del {DEL_B}><w:r><w:delText>Removed row.</w:delText></w:r></w:del></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row three.</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "deleted-row",
        &make_docx_with_body(&body),
        &["Row one.", "Row three.", "Tail."],
        &["Row one.", "Removed row.", "Row three.", "Tail."],
    );
}

#[test]
fn inserted_table_row() {
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4675"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row one.</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:ins {INS_A}/></w:trPr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:ins {INS_A}><w:r><w:t>Added row.</w:t></w:r></w:ins></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row three.</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "inserted-row",
        &make_docx_with_body(&body),
        &["Row one.", "Added row.", "Row three.", "Tail."],
        &["Row one.", "Row three.", "Tail."],
    );
}

#[test]
fn stacked_table_row() {
    // trPr carries BOTH markers: the row drops in both full resolutions
    // (origin rules, Word-oracle-verified in spec_stacked_revisions.rs).
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4675"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row one.</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:ins {INS_A}/><w:del {DEL_B}/></w:trPr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:ins {INS_A}><w:del {DEL_B}><w:r><w:delText>Stacked row.</w:delText></w:r></w:del></w:ins></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row three.</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "stacked-row",
        &make_docx_with_body(&body),
        &["Row one.", "Row three.", "Tail."],
        &["Row one.", "Row three.", "Tail."],
    );
}

fn two_cell_table(second_tc_pr_extra: &str, second_cell_para: &str) -> String {
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Keep.</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/>{second_tc_pr_extra}</w:tcPr>{second_cell_para}</w:tc></w:tr></w:tbl><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    )
}

#[test]
fn inserted_table_cell() {
    let body = two_cell_table(
        &format!(r#"<w:cellIns {INS_A}/>"#),
        r#"<w:p><w:r><w:t>Added cell.</w:t></w:r></w:p>"#,
    );
    check_cross_path(
        "inserted-cell",
        &make_docx_with_body(&body),
        &["Keep.", "Added cell.", "Tail."],
        &["Keep.", "Tail."],
    );
}

#[test]
fn deleted_table_cell() {
    let body = two_cell_table(
        &format!(r#"<w:cellDel {DEL_B}/>"#),
        r#"<w:p><w:r><w:t>Removed cell.</w:t></w:r></w:p>"#,
    );
    check_cross_path(
        "deleted-cell",
        &make_docx_with_body(&body),
        &["Keep.", "Tail."],
        &["Keep.", "Removed cell.", "Tail."],
    );
}

#[test]
fn stacked_table_cell() {
    // tcPr carries BOTH cellIns and cellDel: the contested cell drops in
    // both full resolutions (same origin rules as rows and marks).
    let body = two_cell_table(
        &format!(r#"<w:cellIns {INS_A}/><w:cellDel {DEL_B}/>"#),
        r#"<w:p><w:r><w:t>Contested.</w:t></w:r></w:p>"#,
    );
    check_cross_path(
        "stacked-cell",
        &make_docx_with_body(&body),
        &["Keep.", "Tail."],
        &["Keep.", "Tail."],
    );
}

#[test]
fn ppr_change_formatting() {
    // §17.13.5.29 pPrChange: invisible in the text channel either way, but
    // the projection fails loud if either path leaves the change record (or
    // any tracked state) behind, and the join machinery must not be confused
    // by the extra pPr content.
    let body = format!(
        r#"<w:p><w:pPr><w:jc w:val="center"/><w:pPrChange {INS_A}><w:pPr/></w:pPrChange></w:pPr><w:r><w:t>Centered text.</w:t></w:r></w:p><w:p><w:r><w:t>Plain.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "ppr-change",
        &make_docx_with_body(&body),
        &["Centered text.", "Plain."],
        &["Centered text.", "Plain."],
    );
}

#[test]
fn rpr_change_formatting() {
    // §17.13.5.30 rPrChange on a run (bold toggled). The text channel sees
    // identical text either way (caps is the only formatting the channel
    // renders); both paths must drop the record cleanly.
    let body = format!(
        r#"<w:p><w:r><w:rPr><w:b/><w:rPrChange {INS_A}><w:rPr/></w:rPrChange></w:rPr><w:t>Bold text.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "rpr-change",
        &make_docx_with_body(&body),
        &["Bold text."],
        &["Bold text."],
    );
}

/// §17.13.5.30: rejecting a tracked formatting change must RESTORE the
/// previous run properties the rPrChange record carries — not keep the new
/// ones. The divergence is visible in the text channel exactly when the
/// changed property is `w:caps` (the one formatting bit the channel
/// renders): a reject that keeps the new formatting yields "SHOUT" where
/// the correct projection is "Shout". (This was once a byte-path gap:
/// `reject_all_docx` used to drop `*PrChange` records the same way accept
/// does.)
#[test]
fn rpr_change_caps_reject_restores_previous_formatting() {
    let body = format!(
        r#"<w:p><w:r><w:rPr><w:caps/><w:rPrChange {INS_A}><w:rPr/></w:rPrChange></w:rPr><w:t>Shout</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "rpr-change-caps",
        &make_docx_with_body(&body),
        &["SHOUT"],
        &["Shout"],
    );
}

#[test]
fn ppr_change_with_inserted_paragraph_mark() {
    // pPrChange and an INSERTED paragraph mark on the same pPr: rejecting
    // must BOTH restore the previous paragraph properties (§17.13.5.29) and
    // join the paragraphs (§17.13.5.19). The byte-path restore rebuilds the
    // pPr from the record's payload — if it dropped the mark's w:rPr/w:ins
    // marker in the process, the join would silently stop happening.
    let body = format!(
        r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:ins {INS_A}/></w:rPr><w:pPrChange {INS_A}><w:pPr/></w:pPrChange></w:pPr><w:r><w:t>Alpha</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> beta.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "ppr-change-with-inserted-mark",
        &make_docx_with_body(&body),
        &["Alpha", "beta."],
        &["Alpha beta."],
    );
}

#[test]
fn tracked_changes_inside_hyperlink_display_runs() {
    // w:ins / w:del wrapping runs INSIDE a w:hyperlink. The model path
    // projects HyperlinkData.runs; the byte path unwraps/drops in the XML.
    // The projection renders hyperlink run text (not U+FFFC) so this is
    // actually comparable.
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink w:anchor="bm1"><w:r><w:t xml:space="preserve">the link</w:t></w:r><w:ins {INS_A}><w:r><w:t xml:space="preserve"> appended</w:t></w:r></w:ins><w:del {DEL_B}><w:r><w:delText xml:space="preserve"> removed</w:delText></w:r></w:del></w:hyperlink><w:r><w:t>.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "hyperlink-tracked-runs",
        &make_docx_with_body(&body),
        &["See the link appended."],
        &["See the link removed."],
    );
}

#[test]
fn moved_paragraph_with_range_markers() {
    // A tracked MOVE in real Word shape (mined from the corpus
    // safe-valcap-vs-discount sample): body-level w:moveFromRangeStart/End
    // and w:moveToRangeStart/End delimiters (§17.13.5.24–28), the source
    // paragraph's content in w:moveFrom (delText) with a w:del paragraph
    // mark, the destination paragraph's content in w:moveTo with a w:ins
    // mark. Accept keeps the destination; reject keeps the source. The
    // byte path used to leave the range delimiters behind, which reimported
    // as still-pending block insertions.
    let body = format!(
        r#"<w:p><w:r><w:t>Intro.</w:t></w:r></w:p><w:moveFromRangeStart w:id="10" w:name="move_0" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"/><w:p><w:pPr><w:rPr><w:del w:id="11" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:moveFrom w:id="12" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:delText xml:space="preserve">Moved sentence.</w:delText></w:r></w:moveFrom></w:p><w:moveFromRangeEnd w:id="10"/><w:p><w:r><w:t>Middle.</w:t></w:r></w:p><w:moveToRangeStart w:id="13" w:name="move_0" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"/><w:p><w:pPr><w:rPr><w:ins {INS_A}/></w:rPr></w:pPr><w:moveTo w:id="14" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">Moved sentence.</w:t></w:r></w:moveTo></w:p><w:moveToRangeEnd w:id="13"/><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "moved-paragraph",
        &make_docx_with_body(&body),
        &["Intro.", "Middle.", "Moved sentence.", "Tail."],
        &["Intro.", "Moved sentence.", "Middle.", "Tail."],
    );
}

#[test]
fn tracked_paragraph_inside_table_cell() {
    // The join machinery must also work INSIDE a cell: a deleted paragraph
    // mark on a cell's first paragraph joins it into the cell's second
    // paragraph on accept.
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4675"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:rPr><w:del {DEL_B}/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">Cell first</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> cell second.</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    check_cross_path(
        "cell-deleted-para-mark",
        &make_docx_with_body(&body),
        &["Cell first cell second.", "Tail."],
        &["Cell first", "cell second.", "Tail."],
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Corpus sweep (nightly)
// ═══════════════════════════════════════════════════════════════════════════

fn discover_corpus_docx() -> Vec<PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "docx") {
                out.push(path);
            }
        }
    }
    let samples = common::samples_dir();
    let mut out = Vec::new();
    walk(&samples, &mut out);
    out.sort();
    out
}

/// Known model-path defects surfaced by this sweep, excluded per-file with
/// the classified root cause. These are NOT acceptable behavior — they are
/// real model-path bugs awaiting their own fixes — and entries here must
/// name the defect so the fix can find them.
///
/// Currently EMPTY: both known defect families have been fixed —
/// "prefix-hoist" (18 files; import refuses to hoist tracked label text
/// into the untracked `literal_prefix`, and paragraph-mark joins carry
/// hoisted labels through the merge) and "block-math-in-revision" (3 files;
/// `tracked_change_atoms` now emits widget atoms for paragraph-level
/// widgets like `m:oMathPara` inside `w:ins`/`w:del`). See the
/// `*_literal_prefix_*`, `*prefix*join*`, and `*_block_math` fixtures.
///
/// Entries are matched by path suffix. The comparison still RUNS for
/// excluded files: if a file stops diverging (the underlying defect got
/// fixed), the sweep fails loudly telling you to remove the stale entry.
const KNOWN_MODEL_DEFECT_EXCLUSIONS: &[(&str, &str)] = &[];

/// True when the imported model doc contains a quarantined body item
/// (`OpaqueKind::QuarantinedNestedTracking`). The model PRESERVES such items
/// byte-faithfully with their revisions inside (a documented design decision:
/// the read view shows a placeholder, resolution does not reach inside), while
/// the byte path
/// resolves the markup textually. Cross-path equivalence is therefore not
/// applicable to these documents by design; they are skipped LOUDLY.
fn has_quarantined_blocks(doc: &CanonDoc) -> bool {
    fn block_quarantined(block: &BlockNode) -> bool {
        match block {
            BlockNode::OpaqueBlock(o) => {
                matches!(o.kind, stemma::OpaqueKind::QuarantinedNestedTracking)
            }
            BlockNode::Table(t) => t
                .rows
                .iter()
                .flat_map(|r| r.cells.iter())
                .flat_map(|c| c.blocks.iter())
                .any(block_quarantined),
            BlockNode::Paragraph(_) => false,
        }
    }
    doc.blocks.iter().any(|tb| block_quarantined(&tb.block))
}

/// The cross-path invariant over EVERY *.docx under backend/samples.
/// Files that fail to import are counted and reported (the invariant only
/// applies to importable documents); everything else must agree, except the
/// explicitly classified classes above (quarantine by detection, known
/// model-path defects by per-file entry — both counted and printed, never
/// silent).
#[test]
#[ignore = "stress: requires corpus; set STEMMA_CORPUS_ROOT, run via just nightly"]
fn cross_path_projection_equivalence_corpus_sweep() {
    let files = discover_corpus_docx();
    assert!(!files.is_empty(), "no *.docx found under backend/samples");

    let skipped: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let quarantined: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let known_defects: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let checked = std::sync::atomic::AtomicUsize::new(0);

    files.par_iter().for_each(|path| {
        let name = path.display().to_string();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {name}: {e}"));

        // The invariant applies only to importable docs: skip (and count)
        // files the importer refuses.
        let runtime = SimpleRuntime::new();
        let canon = match runtime.import_docx(&bytes) {
            Ok(import) => std::sync::Arc::unwrap_or_clone(import.canonical),
            Err(e) => {
                let msg = format!("SKIP (import failed): {name}: {e:?}");
                eprintln!("  {msg}");
                skipped.lock().unwrap().push(msg);
                return;
            }
        };

        if has_quarantined_blocks(&canon) {
            let msg = format!(
                "SKIP (quarantined nested tracking — model preserves raw by design): {name}"
            );
            eprintln!("  {msg}");
            quarantined.lock().unwrap().push(msg);
            return;
        }

        let known_defect = KNOWN_MODEL_DEFECT_EXCLUSIONS
            .iter()
            .find(|(suffix, _)| name.ends_with(suffix));

        let result = std::panic::catch_unwind(move || {
            for direction in ["accept", "reject"] {
                let accept = direction == "accept";
                let model = model_projection(&canon, accept, &format!("{name}/{direction}/model"));
                let byte = byte_projection(&bytes, accept, &format!("{name}/{direction}/byte"));
                if model != byte {
                    let first_diff = model
                        .iter()
                        .zip(byte.iter())
                        .position(|(m, b)| m != b)
                        .unwrap_or(model.len().min(byte.len()));
                    return Err(format!(
                        "[{name}] {direction}-all diverges: model {} paras vs byte {} paras; \
                         first differing para #{first_diff}:\n  model: {:?}\n  byte:  {:?}",
                        model.len(),
                        byte.len(),
                        model.get(first_diff),
                        byte.get(first_diff),
                    ));
                }
            }
            Ok(())
        });

        let name = path.display().to_string();
        let divergence: Option<String> = match result {
            Ok(Ok(())) => None,
            Ok(Err(msg)) => Some(msg),
            Err(panic_info) => Some(if let Some(s) = panic_info.downcast_ref::<String>() {
                format!("[{name}] PANIC: {s}")
            } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                format!("[{name}] PANIC: {s}")
            } else {
                format!("[{name}] PANIC: (unknown payload)")
            }),
        };

        match (divergence, known_defect) {
            (None, None) => {
                checked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            (None, Some((suffix, reason))) => {
                // The defect this entry documents no longer manifests — the
                // exclusion is stale and must be removed, or it would mask a
                // future regression on this file.
                let msg = format!(
                    "[{name}] STALE EXCLUSION: paths now AGREE but the file is \
                     excluded as a known model defect ({reason}). Remove the \
                     {suffix:?} entry from KNOWN_MODEL_DEFECT_EXCLUSIONS."
                );
                eprintln!("  {msg}");
                failures.lock().unwrap().push(msg);
            }
            (Some(msg), Some((_, reason))) => {
                let msg = format!("KNOWN MODEL DEFECT ({reason}): {msg}");
                eprintln!("  {msg}");
                known_defects.lock().unwrap().push(msg);
            }
            (Some(msg), None) => {
                eprintln!("  DIVERGE: {msg}");
                failures.lock().unwrap().push(msg);
            }
        }
    });

    let skipped = skipped.into_inner().unwrap();
    let quarantined = quarantined.into_inner().unwrap();
    let known_defects = known_defects.into_inner().unwrap();
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    eprintln!(
        "cross_path_projection_equivalence_corpus_sweep: {} files, {} agree, \
         {} skipped (import failed), {} skipped (quarantined nested tracking), \
         {} known model defects (excluded, still diverging), {} UNEXPLAINED divergent",
        files.len(),
        checked.load(std::sync::atomic::Ordering::Relaxed),
        skipped.len(),
        quarantined.len(),
        known_defects.len(),
        failures.len(),
    );

    assert!(
        failures.is_empty(),
        "cross-path divergences on {} of {} importable files:\n{}",
        failures.len(),
        files.len() - skipped.len(),
        failures.join("\n"),
    );
}
