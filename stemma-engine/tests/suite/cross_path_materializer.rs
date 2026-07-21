//! Cross-path materializer characterization (guards "Invariant M").
//!
//! Stemma has TWO materializers that lower a paragraph text change into
//! tracked-change `TrackedSegment`s:
//!
//!   - EDIT path: `apply_transaction(&CanonDoc, &EditTransaction)` (engine
//!     stemma-engine/src/edit.rs).
//!   - MERGE path: `diff_documents(&CanonDoc, &CanonDoc)` then
//!     `merge_diff(&a, &b, &diff, &rev)` (engine stemma-engine/src/tracked_model.rs).
//!
//! These are slated to be unified into a single routine ("Invariant M":
//! the same logical change lowered through either path must produce the
//! same tracked-segment structure on the target paragraph). Before that
//! refactor lands we need a safety net that drives the SAME logical change
//! through BOTH paths and compares the resulting segment structure.
//!
//! This test is that safety net AND a characterization of how the two
//! paths relate today. The comparison is intentionally STRICT (status
//! discriminant + concatenated text + ordered inline kinds, modulo
//! revision identity). If the two paths match on a fixture, this test
//! asserts equivalence and runs daily. If they diverge, the divergence is
//! captured precisely (printed catalogue + the assertion message) and the
//! test is `#[ignore]`d as a characterization pending the unification — the
//! assertion is NOT weakened to force green.
//!
//! ── Path equivalence: localized first-word replacement ───────────────────
//!
//! For a localized first-word REPLACEMENT (replace the first word with a
//! different word, keep the rest identical), the two materializers AGREE on
//! every usable in-tree fixture under the strict projection: same segment
//! count, same Deleted/Inserted/Normal partitioning, same text per segment,
//! same ordered inline kinds. Concretely both paths lower the change to:
//!
//!     [0] Deleted  kinds=[Text]              text="This"
//!     [1] Inserted kinds=[Text]              text="REPLACED"
//!     [2] Normal   kinds=[Text, ...]         text=" is a ... <tail>"
//!
//! (The trailing `Other` inline kinds on the Normal segment of the
//! `simple-text` fixture are zero-width decoration/comment markers that BOTH
//! paths carry identically — they do not affect equivalence.)
//!
//! Because they MATCH, this is a PASSING daily test asserting equivalence.
//! It guards Invariant M directly: the unification must keep both paths
//! producing this same projection. Should a future change cause the paths
//! to diverge, the assertion at the bottom fails loudly with a side-by-side
//! catalogue — at which point the test should be `#[ignore]`d (with the
//! divergence enumerated) until the unification re-converges them, NOT
//! weakened to pass.
//!
//! NOTE on fixture coverage: `testdata/twenty-paragraphs/before.docx` has
//! only single-word paragraphs ("One", "Two", ...), so it has no editable
//! paragraph under the >=2-word rule and is explicitly skipped (loud
//! `eprintln`, not a swallowed error). The curated set still exercises two
//! usable fixtures (`simple-text`, `paragraphs`), and the test asserts the
//! usable set is non-empty.

use std::fs;

use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    apply_transaction,
};
use stemma::{
    BlockNode, CanonDoc, DocxRuntime, InlineNode, NodeId, OpaqueInlineNode, ParagraphNode,
    RevisionInfo, SimpleRuntime, StyleProps, TextNode, TrackedSegment, TrackingStatus,
    diff_documents, merge_diff, normal_segment,
};

// ── shared revision ───────────────────────────────────────────────────────

/// A single fixed revision used for BOTH paths so revision-id numbering can
/// never be the source of a divergence (the projection ignores it anyway,
/// but pinning it keeps the two lowerings as comparable as possible).
fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 7,
        identity: 0,
        author: Some("cross-path".to_string()),
        date: Some("2026-05-31T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

// ── editable-paragraph discovery (copied verbatim from
//    stemma-engine/tests/edit_invariants.rs — test files are separate
//    compilation units, so the helper is copied, not imported) ────────────

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
        if p.segments.iter().any(|s| {
            s.inlines
                .iter()
                .any(|i| matches!(i, InlineNode::OpaqueInline(_) | InlineNode::HardBreak(_)))
        }) {
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
        if text.split_whitespace().count() >= 2 && !first_word.is_empty() {
            Some((p.id.clone(), text, first_word))
        } else {
            None
        }
    })
}

// ── locating the target paragraph in a (possibly edited/merged) doc ───────

fn find_para<'a>(doc: &'a CanonDoc, block_id: &NodeId) -> &'a ParagraphNode {
    doc.blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) if &p.id == block_id => Some(p),
            _ => None,
        })
        .unwrap_or_else(|| panic!("paragraph '{block_id}' not found"))
}

// ── normalized projection of a TrackedSegment ─────────────────────────────

/// Status discriminant only — NOT the `RevisionInfo` payload. Materializer
/// equivalence is about WHETHER a span is inserted/deleted/normal, not which
/// revision id Word will stamp on it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StatusKind {
    Normal,
    Inserted,
    Deleted,
}

fn status_kind(status: &TrackingStatus) -> StatusKind {
    match status {
        TrackingStatus::Normal => StatusKind::Normal,
        TrackingStatus::Inserted(_) => StatusKind::Inserted,
        TrackingStatus::Deleted(_) => StatusKind::Deleted,
        // The cross-path comparison runs the merge and edit pipelines, which
        // never produce stacked segments.
        TrackingStatus::InsertedThenDeleted(_) => {
            unreachable!("merge/edit cross-path fixtures never produce stacked segments")
        }
    }
}

/// Inline KIND discriminant (no payload). The materializers must agree on
/// the ordered shape of each segment's inlines.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InlineKind {
    Text,
    OpaqueInline,
    HardBreak,
    /// Zero-width / comment markers — not expected on these plain fixtures,
    /// but captured rather than silently dropped (no silent fallbacks).
    Other,
}

fn inline_kind(inline: &InlineNode) -> InlineKind {
    match inline {
        InlineNode::Text(_) => InlineKind::Text,
        InlineNode::OpaqueInline(_) => InlineKind::OpaqueInline,
        InlineNode::HardBreak(_) => InlineKind::HardBreak,
        InlineNode::Decoration(_)
        | InlineNode::CommentRangeStart { .. }
        | InlineNode::CommentRangeEnd { .. }
        | InlineNode::CommentReference { .. } => InlineKind::Other,
    }
}

/// What matters for materializer equivalence, modulo revision identity:
///   - the status DISCRIMINANT (Normal / Inserted / Deleted),
///   - the concatenated text of the segment's text inlines,
///   - the ordered list of inline KINDS.
///
/// Marks/style are intentionally OMITTED for now: the brief says start
/// WITHOUT marks and add them only if both paths populate them, to avoid
/// false diffs. The paths already diverge on status+text+kinds, so marks
/// would only add noise to an already-failing comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedSeg {
    status: StatusKind,
    text: String,
    inline_kinds: Vec<InlineKind>,
}

fn project_segment(seg: &TrackedSegment) -> NormalizedSeg {
    let text: String = seg
        .inlines
        .iter()
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    NormalizedSeg {
        status: status_kind(&seg.status),
        text,
        inline_kinds: seg.inlines.iter().map(inline_kind).collect(),
    }
}

fn project_segments(segments: &[TrackedSegment]) -> Vec<NormalizedSeg> {
    segments.iter().map(project_segment).collect()
}

fn render_projection(segs: &[NormalizedSeg]) -> String {
    let mut out = String::new();
    for (i, s) in segs.iter().enumerate() {
        out.push_str(&format!(
            "    [{i}] {:?} kinds={:?} text={:?}\n",
            s.status, s.inline_kinds, s.text
        ));
    }
    if out.is_empty() {
        out.push_str("    <no segments>\n");
    }
    out
}

// ── building canon_b for the MERGE path ───────────────────────────────────

/// Clone `canon_a` and replace the first word while retaining the source text
/// nodes. Zero-width decorations are omitted from this synthetic target, as in
/// the original characterization: the materializer carries those base anchors.
/// The text-node boundaries are retained because they are part of the layout
/// surface this comparison now guards.
fn build_target_doc(
    canon_a: &CanonDoc,
    block_id: &NodeId,
    old_word: &str,
    new_word: &str,
    expected_text: &str,
) -> CanonDoc {
    let mut canon_b = canon_a.clone();
    let mut replaced = false;
    for tb in &mut canon_b.blocks {
        if let BlockNode::Paragraph(p) = &mut tb.block
            && &p.id == block_id
        {
            let mut text_inlines: Vec<InlineNode> = p
                .segments
                .iter()
                .flat_map(|segment| segment.inlines.iter())
                .filter_map(|inline| match inline {
                    InlineNode::Text(text) => Some(InlineNode::Text(text.clone())),
                    _ => None,
                })
                .collect();
            for inline in &mut text_inlines {
                if let InlineNode::Text(text) = inline
                    && text.text.contains(old_word)
                {
                    text.text = text.text.replacen(old_word, new_word, 1);
                    replaced = true;
                    break;
                }
            }
            let actual_text: String = text_inlines
                .iter()
                .filter_map(|inline| match inline {
                    InlineNode::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                actual_text, expected_text,
                "build_target_doc: first word must be replaceable inside one existing text node"
            );
            p.segments = normal_segment(text_inlines);
            // The text content changed — invalidate the import-time caches so
            // the diff engine recomputes from the new inlines rather than a
            // stale hash/rendered string. (No silent fallback: we don't want
            // the diff to see the OLD text via a cache.)
            p.block_text_hash = None;
            p.rendered_text = None;
            break;
        }
    }
    assert!(
        replaced,
        "build_target_doc: target paragraph '{block_id}' not found in canon_a clone"
    );
    canon_b
}

// ── per-fixture cross-path comparison ─────────────────────────────────────

struct FixtureOutcome {
    fixture: String,
    block_id: String,
    old_text: String,
    new_text: String,
    edit_segs: Vec<NormalizedSeg>,
    merge_segs: Vec<NormalizedSeg>,
    matched: bool,
}

/// Drive ONE fixture through both paths. Returns `None` only when the
/// fixture file is missing from this in-tree checkout (an explicit,
/// loud skip — corpus fixtures are gitignored). Every other failure
/// (import error, no editable paragraph, apply error, diff/merge error)
/// panics: we do NOT best-effort past an unknown state.
fn run_fixture(fixture: &str) -> Option<FixtureOutcome> {
    let Ok(bytes) = fs::read(fixture) else {
        eprintln!("SKIP (missing in-tree fixture): {fixture}");
        return None;
    };

    let runtime = SimpleRuntime::new();
    let canon_a = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {fixture}: {e:?}"))
        .canonical;

    let Some((block_id, old_text, first_word)) = find_editable_paragraph(&canon_a) else {
        eprintln!(
            "SKIP (no editable paragraph): {fixture} — needs a plain-text \
             paragraph with >= 2 words, all-Normal, no opaque/hardbreak inlines"
        );
        return None;
    };

    // Same logical change for BOTH paths: replace the FIRST WORD with a
    // different word, keep the rest identical so the word-diff produces a
    // localized change. A true replacement (not a prepend) is the more
    // discriminating shape: it forces the materializers to emit BOTH a
    // deletion (old first word) and an insertion (new first word), so any
    // disagreement about how those spans abut the unchanged tail surfaces.
    let new_text = old_text.replacen(&first_word, "REPLACED", 1);
    assert_ne!(
        new_text, old_text,
        "{fixture}: first-word replacement must actually change the text"
    );

    let revision = test_revision();

    // ── EDIT path ────────────────────────────────────────────────────────
    let txn = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            expect: old_text.clone(),
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(new_text.clone())],
            },
            rationale: None,
            replacement_role: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision.clone(),
    };
    let (edited, _pending) = apply_transaction(&canon_a, &txn)
        .unwrap_or_else(|e| panic!("apply_transaction {fixture} block {block_id}: {e}"));
    let edit_segs = project_segments(&find_para(&edited, &block_id).segments);

    // ── MERGE path ───────────────────────────────────────────────────────
    let canon_b = build_target_doc(&canon_a, &block_id, &first_word, "REPLACED", &new_text);
    let diff = diff_documents(&canon_a, &canon_b)
        .unwrap_or_else(|e| panic!("diff_documents {fixture} block {block_id}: {e}"));
    let merged = merge_diff(&canon_a, &canon_b, &diff, &revision)
        .unwrap_or_else(|e| panic!("merge_diff {fixture} block {block_id}: {e:?}"))
        .doc;
    let merge_segs = project_segments(&find_para(&merged, &block_id).segments);

    let matched = edit_segs == merge_segs;

    Some(FixtureOutcome {
        fixture: fixture.to_string(),
        block_id: block_id.to_string(),
        old_text,
        new_text,
        edit_segs,
        merge_segs,
        matched,
    })
}

// ── the test ──────────────────────────────────────────────────────────────

/// Curated in-tree fixtures. These ship in the repo (small, committed);
/// corpus fixtures are gitignored, hence the explicit per-file skip in
/// `run_fixture`. The list is asserted non-empty after running so an empty
/// curated set can't make this test vacuously pass.
const FIXTURES: &[&str] = &[
    "testdata/simple-text/before.docx",
    "testdata/twenty-paragraphs/before.docx",
    "testdata/paragraphs/before.docx",
];

/// Lower the SAME logical change (first-word replacement) through the EDIT
/// path and the MERGE path, and compare the target paragraph's tracked
/// segments modulo revision identity.
///
/// This guards "Invariant M": the unified materializer must make BOTH
/// paths produce the same `NormalizedSeg` projection. Today they AGREE on
/// the usable in-tree fixtures (see the module doc-comment), so this is a
/// PASSING daily test. If a future change makes them diverge, the assertion
/// fails with a side-by-side catalogue — `#[ignore]` it then (enumerating
/// the divergence) rather than weakening the comparison.
#[test]
fn cross_path_materializer_equivalence() {
    let mut outcomes: Vec<FixtureOutcome> = Vec::new();
    for fixture in FIXTURES {
        if let Some(outcome) = run_fixture(fixture) {
            outcomes.push(outcome);
        }
    }

    assert!(
        !outcomes.is_empty(),
        "no curated fixture was usable — every fixture in FIXTURES was \
         missing or had no editable paragraph. The cross-path comparison \
         needs at least one usable in-tree fixture."
    );

    // Print the full catalogue regardless of pass/fail. This is the
    // deliverable: a side-by-side projection of both paths per fixture.
    let mut diverged = Vec::new();
    for o in &outcomes {
        eprintln!("\n══════════════════════════════════════════════════════════════");
        eprintln!("FIXTURE: {}", o.fixture);
        eprintln!("  target block: {}", o.block_id);
        eprintln!("  old text: {:?}", o.old_text);
        eprintln!("  new text: {:?}", o.new_text);
        eprintln!(
            "  EDIT  path: {} segment(s)\n{}",
            o.edit_segs.len(),
            render_projection(&o.edit_segs)
        );
        eprintln!(
            "  MERGE path: {} segment(s)\n{}",
            o.merge_segs.len(),
            render_projection(&o.merge_segs)
        );
        if o.matched {
            eprintln!("  RESULT: MATCH ✓");
        } else {
            eprintln!("  RESULT: DIVERGE ✗");
            diverged.push(o.fixture.clone());
        }
    }

    eprintln!("\n══════════════════════════════════════════════════════════════");
    eprintln!(
        "SUMMARY: {} fixture(s) checked, {} matched, {} diverged",
        outcomes.len(),
        outcomes.len() - diverged.len(),
        diverged.len()
    );
    if !diverged.is_empty() {
        eprintln!("DIVERGED: {}", diverged.join(", "));
    }

    // The Invariant-M post-condition: every usable fixture must match.
    // This assertion is the safety net. It currently PASSES (the two
    // materializers agree on these fixtures); the unification must keep it
    // passing. If it ever fails, the message below is the divergence
    // catalogue — `#[ignore]` the test (enumerating the divergence) rather
    // than weakening the projection.
    for o in &outcomes {
        assert_eq!(
            o.edit_segs,
            o.merge_segs,
            "Invariant M violated on {} (block {}):\n  \
             EDIT  path projection:\n{}  \
             MERGE path projection:\n{}",
            o.fixture,
            o.block_id,
            render_projection(&o.edit_segs),
            render_projection(&o.merge_segs),
        );
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Opaque-bearing variant — extends Invariant-M coverage to paragraphs that
// carry a PRESERVED opaque inline (footnote ref / equation / image), which
// domain-model.md §6 lists as a known gap ("Opaque inline changed").
//
// WHY a SYNTHESIZED base instead of editing fixtures in place:
//   Every opaque-bearing paragraph in the in-tree opaque fixtures
//   (`footnotes`, `math-equations`, `images`, `image-math-combined`) holds
//   the opaque ALONE — the OmmlBlock / Drawing widget sits in its own
//   paragraph, flanked at most by zero-width `Decoration` markers, never by
//   visible `Text`. (Verified by inspecting every paragraph of all four
//   fixtures: not one has both a `Text` inline and an `OpaqueInline`.) So
//   the literal "edit the first word BEFORE the opaque" change has no
//   natural host paragraph.
//
//   Rather than skip vacuously (forbidden — "assert the usable set is
//   non-empty so it can't vacuously pass"), the finder locates a REAL
//   opaque inline (genuine `raw_xml`, `kind`, `content_hash` — the exact
//   node that triggers the merge path's `coalesce_split_field_sequences` /
//   `normalize_paragraph_opaque_reading_order` passes), and the runner
//   SYNTHESIZES the base paragraph as `Text(before) + Opaque(clone) +
//   Text(after)`. BOTH materializers then start from that SAME synthesized
//   base, so the comparison is apples-to-apples; the only thing that varies
//   between the two runs is which materializer lowers the first-word
//   replacement.
// ══════════════════════════════════════════════════════════════════════════

/// Fixed text wrapped around the preserved opaque in the synthesized base.
/// Two+ words before the opaque (so the FIRST word can be replaced while a
/// word survives) and text after it (so the post-opaque section is non-empty
/// and any abutment disagreement around the anchor surfaces).
const OPAQUE_TEXT_BEFORE: &str = "This is before";
const OPAQUE_TEXT_AFTER: &str = " and this is after";

/// A real opaque inline lifted from a fixture, plus the synthesized inline
/// arrangement the two paths will share.
struct OpaqueTarget {
    /// Block id of the host paragraph in the synthesized base doc.
    block_id: NodeId,
    /// The genuine opaque node (cloned from the fixture, preserved exactly).
    opaque: OpaqueInlineNode,
}

/// Locate a paragraph that (a) is a Normal block with all-Normal segments and
/// (b) contains at least one `InlineNode::OpaqueInline`. Returns the host
/// block id and a CLONE of the first opaque inline found.
///
/// NOTE: the task's literal requirement — ">= 2 words of `Text` BEFORE the
/// first opaque" — cannot be met by any in-tree opaque fixture (opaques are
/// always isolated in their own paragraph; see the module section comment
/// above). The text-before / opaque / text-after arrangement the two paths
/// need is therefore SYNTHESIZED by `build_opaque_base_doc` from the real
/// opaque this finder returns, not read from the paragraph as-is.
fn find_opaque_bearing_paragraph(doc: &CanonDoc) -> Option<OpaqueTarget> {
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
        let opaque = p
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .find_map(|i| match i {
                InlineNode::OpaqueInline(o) => Some((**o).clone()),
                _ => None,
            })?;
        Some(OpaqueTarget {
            block_id: p.id.clone(),
            opaque,
        })
    })
}

/// Build the synthesized base doc: clone `canon_a` and rebuild the target
/// paragraph's segments to a single Normal segment laid out as
/// `Text(before) + Opaque(clone) + Text(after)`. The opaque is the genuine
/// fixture node, preserved exactly. Import-time caches are cleared so the
/// diff engine reads the new inlines (mirrors `build_target_doc`).
fn build_opaque_base_doc(
    canon_a: &CanonDoc,
    block_id: &NodeId,
    opaque: &OpaqueInlineNode,
    text_before: &str,
    text_after: &str,
) -> CanonDoc {
    let mut doc = canon_a.clone();
    let mut replaced = false;
    for tb in &mut doc.blocks {
        if let BlockNode::Paragraph(p) = &mut tb.block
            && &p.id == block_id
        {
            let inlines = vec![
                opaque_base_text(block_id, "before", text_before),
                InlineNode::from(opaque.clone()),
                opaque_base_text(block_id, "after", text_after),
            ];
            p.segments = normal_segment(inlines);
            p.block_text_hash = None;
            p.rendered_text = None;
            replaced = true;
            break;
        }
    }
    assert!(
        replaced,
        "build_opaque_base_doc: target paragraph '{block_id}' not found"
    );
    doc
}

/// Build the MERGE-path target doc from the SYNTHESIZED base: same opaque,
/// same text-after, but `text_before` edited (first word replaced). Caches
/// cleared so the diff reads the new inlines.
fn build_opaque_target_doc(
    base: &CanonDoc,
    block_id: &NodeId,
    opaque: &OpaqueInlineNode,
    edited_text_before: &str,
    text_after: &str,
) -> CanonDoc {
    build_opaque_base_doc(base, block_id, opaque, edited_text_before, text_after)
}

fn opaque_base_text(block_id: &NodeId, slot: &str, text: &str) -> InlineNode {
    InlineNode::from(TextNode {
        id: NodeId::from(format!("{}__{slot}", block_id.0)),
        text_role: None,
        text: text.to_string(),
        marks: vec![],
        style_props: StyleProps::default(),
        rpr_authored: stemma::domain::RunRprAuthored::default(),
        source_run_attrs: Vec::new(),
        formatting_change: None,
    })
}

/// Outcome of driving one opaque fixture through both paths. Carries the same
/// side-by-side projections as `FixtureOutcome`, plus any EDIT-path engine
/// error (e.g. `OpaqueDestroyed`) captured verbatim instead of papered over.
struct OpaqueFixtureOutcome {
    fixture: String,
    block_id: String,
    old_text: String,
    new_text: String,
    edit_result: Result<Vec<NormalizedSeg>, String>,
    merge_segs: Vec<NormalizedSeg>,
    matched: bool,
}

fn run_opaque_fixture(fixture: &str) -> Option<OpaqueFixtureOutcome> {
    let Ok(bytes) = fs::read(fixture) else {
        eprintln!("SKIP (missing in-tree fixture): {fixture}");
        return None;
    };

    let runtime = SimpleRuntime::new();
    let canon_raw = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {fixture}: {e:?}"))
        .canonical;

    let Some(target) = find_opaque_bearing_paragraph(&canon_raw) else {
        eprintln!(
            "SKIP (no opaque-bearing paragraph): {fixture} — needs a Normal \
             paragraph with all-Normal segments containing an OpaqueInline"
        );
        return None;
    };
    let block_id = target.block_id.clone();

    // The SHARED synthesized base: Text(before) + Opaque + Text(after).
    let base = build_opaque_base_doc(
        &canon_raw,
        &block_id,
        &target.opaque,
        OPAQUE_TEXT_BEFORE,
        OPAQUE_TEXT_AFTER,
    );

    // The same logical change for BOTH paths: replace the FIRST WORD of the
    // text BEFORE the opaque, keeping the opaque and the text-after intact.
    let first_word = OPAQUE_TEXT_BEFORE
        .split_whitespace()
        .next()
        .expect("OPAQUE_TEXT_BEFORE has >= 2 words");
    let edited_before = OPAQUE_TEXT_BEFORE.replacen(first_word, "REPLACED", 1);
    assert_ne!(
        edited_before, OPAQUE_TEXT_BEFORE,
        "{fixture}: first-word replacement must change the text-before"
    );

    // Full visible text of the base paragraph (opaque contributes none):
    // before + after, concatenated, as `paragraph_visible_text` computes it.
    let old_text = format!("{OPAQUE_TEXT_BEFORE}{OPAQUE_TEXT_AFTER}");
    let new_text = format!("{edited_before}{OPAQUE_TEXT_AFTER}");

    let revision = test_revision();

    // ── EDIT path ────────────────────────────────────────────────────────
    // Fragments preserve the opaque by reference between the two text spans.
    // `expect` must be a substring of a text SECTION (text between anchors);
    // the edited first word lives in the BEFORE section, so the BEFORE text
    // is the right anchor-bounded expectation (the opaque contributes no
    // visible text, so the full paragraph text would NOT be a single
    // section). See `extract_text_sections` / `validate_replace_step`.
    let txn = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            expect: OPAQUE_TEXT_BEFORE.to_string(),
            content: ParagraphContent {
                fragments: vec![
                    ContentFragment::Text(edited_before.clone()),
                    ContentFragment::PreservedInlineRef(target.opaque.id.clone()),
                    ContentFragment::Text(OPAQUE_TEXT_AFTER.to_string()),
                ],
            },
            rationale: None,
            replacement_role: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision.clone(),
    };
    // Capture an engine error (e.g. OpaqueDestroyed, ExpectMismatch) verbatim
    // rather than panicking: an error IS information about the divergence we
    // are cataloguing. The comparison is still performed when EDIT succeeds.
    let edit_result: Result<Vec<NormalizedSeg>, String> = match apply_transaction(&base, &txn) {
        Ok((edited, _pending)) => Ok(project_segments(&find_para(&edited, &block_id).segments)),
        Err(e) => Err(format!("{e}")),
    };

    // ── MERGE path ───────────────────────────────────────────────────────
    let canon_b = build_opaque_target_doc(
        &base,
        &block_id,
        &target.opaque,
        &edited_before,
        OPAQUE_TEXT_AFTER,
    );
    let diff = diff_documents(&base, &canon_b)
        .unwrap_or_else(|e| panic!("diff_documents {fixture} block {block_id}: {e}"));
    let merged = merge_diff(&base, &canon_b, &diff, &revision)
        .unwrap_or_else(|e| panic!("merge_diff {fixture} block {block_id}: {e:?}"))
        .doc;
    let merge_segs = project_segments(&find_para(&merged, &block_id).segments);

    let matched = match &edit_result {
        Ok(edit_segs) => *edit_segs == merge_segs,
        Err(_) => false,
    };

    Some(OpaqueFixtureOutcome {
        fixture: fixture.to_string(),
        block_id: block_id.to_string(),
        old_text,
        new_text,
        edit_result,
        merge_segs,
        matched,
    })
}

/// Opaque-bearing in-tree fixtures. Each ships a paragraph whose only inline
/// is a real opaque widget (image / equation), which the runner wraps in
/// synthesized text. `footnotes/before.docx` is included even though its
/// footnote reference lives in the footnote story (its body paragraphs carry
/// no opaque inline) — it then hits the explicit "no opaque-bearing
/// paragraph" skip rather than being silently dropped.
const OPAQUE_FIXTURES: &[&str] = &[
    "testdata/footnotes/before.docx",
    "testdata/math-equations/before.docx",
    "testdata/images/before.docx",
    "testdata/image-math-combined/before.docx",
];

/// Lower the SAME first-word replacement through the EDIT path and the MERGE
/// path on a paragraph that CONTAINS a preserved opaque inline, and compare
/// the target paragraph's tracked segments modulo revision identity.
///
/// This extends Invariant-M coverage to opaque-bearing paragraphs
/// (domain-model.md §6 "Opaque inline changed"). The merge path runs opaque
/// normalization passes (`coalesce_split_field_sequences`,
/// `normalize_paragraph_opaque_reading_order`) that the edit path does not,
/// so this is exactly where the two materializers were most likely to
/// diverge — making a MATCH here a meaningful guarantee, not a freebie.
///
/// ── Path equivalence: first-word replacement before an opaque inline ─────
///
/// For a localized first-word REPLACEMENT on text BEFORE a preserved opaque
/// inline (keeping the opaque and the text-after intact), the two
/// materializers AGREE on every usable in-tree opaque fixture under the
/// strict projection. Both paths lower the change to:
///
///     [0] Deleted  kinds=[Text]                     text="This"
///     [1] Inserted kinds=[Text]                     text="REPLACED"
///     [2] Normal   kinds=[Text, OpaqueInline, Text] text=" is before and this is after"
///
/// Crucially, BOTH paths keep the opaque in its original reading position
/// inside the unchanged tail segment, with the same text partition on either
/// side of it. The merge path's opaque normalization passes do NOT perturb
/// this relative to the edit path's anchor-aware reconstruction. So §6's
/// "Opaque inline changed" gap does NOT manifest as a cross-path divergence
/// for the localized-replacement-around-a-preserved-opaque shape.
///
/// Because they MATCH, this is a PASSING daily test asserting equivalence; it
/// extends the Invariant-M safety net to opaque-bearing paragraphs. If a
/// future change makes the paths diverge on opaques, the assertion fails with
/// a side-by-side catalogue (including any EDIT-path engine error such as
/// `OpaqueDestroyed`) — `#[ignore]` it then (enumerating the divergence)
/// rather than weakening the projection.
///
/// ── On fixture realism (SYNTHESIZED base) ────────────────────────────────
///
/// None of the in-tree opaque fixtures host a real opaque inline ALONGSIDE
/// visible text in the same paragraph, so the base paragraph is synthesized
/// around a GENUINE fixture opaque (see the section comment above
/// `find_opaque_bearing_paragraph`). The opaque is real (its `raw_xml` /
/// `kind` / `content_hash` are what drive the merge-path normalization
/// passes); only the surrounding text is synthetic, and it is IDENTICAL for
/// both paths, so the comparison stays apples-to-apples.
#[test]
fn cross_path_materializer_equivalence_opaque() {
    let mut outcomes: Vec<OpaqueFixtureOutcome> = Vec::new();
    for fixture in OPAQUE_FIXTURES {
        if let Some(outcome) = run_opaque_fixture(fixture) {
            outcomes.push(outcome);
        }
    }

    assert!(
        !outcomes.is_empty(),
        "no opaque fixture was usable — every fixture in OPAQUE_FIXTURES was \
         missing or had no opaque-bearing paragraph. The opaque cross-path \
         comparison needs at least one usable in-tree fixture."
    );

    let mut diverged = Vec::new();
    for o in &outcomes {
        eprintln!("\n══════════════════════════════════════════════════════════════");
        eprintln!("FIXTURE: {}", o.fixture);
        eprintln!("  target block: {}", o.block_id);
        eprintln!("  old text: {:?}", o.old_text);
        eprintln!("  new text: {:?}", o.new_text);
        match &o.edit_result {
            Ok(edit_segs) => eprintln!(
                "  EDIT  path: {} segment(s)\n{}",
                edit_segs.len(),
                render_projection(edit_segs)
            ),
            Err(err) => eprintln!("  EDIT  path: ENGINE ERROR: {err}"),
        }
        eprintln!(
            "  MERGE path: {} segment(s)\n{}",
            o.merge_segs.len(),
            render_projection(&o.merge_segs)
        );
        if o.matched {
            eprintln!("  RESULT: MATCH ✓");
        } else {
            eprintln!("  RESULT: DIVERGE ✗");
            diverged.push(o.fixture.clone());
        }
    }

    eprintln!("\n══════════════════════════════════════════════════════════════");
    eprintln!(
        "SUMMARY: {} fixture(s) checked, {} matched, {} diverged",
        outcomes.len(),
        outcomes.len() - diverged.len(),
        diverged.len()
    );
    if !diverged.is_empty() {
        eprintln!("DIVERGED: {}", diverged.join(", "));
    }

    // Invariant-M post-condition for opaque-bearing paragraphs: every usable
    // fixture must match. This currently PASSES (the two materializers agree
    // on opaques for the localized-replacement shape). If it ever fails, the
    // message below is the divergence catalogue (including any captured
    // EDIT-path engine error) — `#[ignore]` the test then, enumerating the
    // divergence, rather than weakening the projection to force green.
    for o in &outcomes {
        let edit_render = match &o.edit_result {
            Ok(edit_segs) => render_projection(edit_segs),
            Err(err) => format!("    <engine error: {err}>\n"),
        };
        assert!(
            o.matched,
            "Invariant M (opaque) violated on {} (block {}):\n  \
             EDIT  path projection:\n{}  \
             MERGE path projection:\n{}",
            o.fixture,
            o.block_id,
            edit_render,
            render_projection(&o.merge_segs),
        );
    }
}
