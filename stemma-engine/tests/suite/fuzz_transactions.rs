//! Property/fuzz transaction-fidelity sweep — the Word-free fidelity tier (roadmap D).
//!
//! WHAT THIS IS. A deterministic generator emits RANDOM-but-VALID multi-verb v4
//! `EditTransaction`s over a small pool of seed documents, and for every
//! generated transaction asserts the canonical Word-free fidelity invariants —
//! the exact properties that earn OSS fidelity confidence WITHOUT a Word oracle
//! or an external corpus. It is the at-scale, empirical companion to the
//! per-verb `edit_fidelity_invariants.rs` gate: that file proves each verb in
//! isolation on a hand-built case; this file proves arbitrary COMPOSITIONS of
//! verbs hold the same invariants across thousands of seeds.
//!
//! THE INVARIANTS (per generated transaction `T` applied to base `B`):
//!   I1. reject_all(apply_tracked(B,T)) ≡ B, modulo comment-anchor markers
//!       (reversibility / reject==baseline). `CommentCreate` is an
//!       ANNOTATION verb, not a tracked change (`edit/verbs/comments.rs`):
//!       its three markers are spliced as zero-width Normal decorations even
//!       in TrackedChange mode, so `reject_all` — which only resolves
//!       TRACKED deltas — correctly leaves them in place. I1 therefore
//!       compares `shape()` with comment-anchor lines stripped
//!       (`shape_for_reversibility`); `edit_fidelity_invariants.rs`'s
//!       `comment_create_is_an_annotation_surviving_accept_and_reject` proves
//!       the markers themselves survive, on the IR directly.
//!   I2. accept_all(apply_tracked(B,T)) ≡ apply_direct(B,T) (accept==direct)
//!   I3. serialize(redline) under the BLOCKING validator emits zero blocking
//!       findings (the serialized OOXML is structurally sound)
//!   I4. the opaque/anchor inventory is non-shrinking (fail-never preservation)
//!   I5. parse(serialize(redline)) round-trips structurally (the bytes re-decode
//!       to the same shape)
//!   I6. atomicity: a transaction whose LAST step is guaranteed-stale fails
//!       whole, leaving the base untouched (a failing step rolls back)
//!
//! `≡` is the engine-independent content fingerprint `shape()` (block role +
//! style + paragraph formatting + per-segment visible text, marks, and tracked
//! status), exactly as in `edit_fidelity_invariants.rs`.
//!
//! DETERMINISM. Each case is identified by an integer `seed`. A pure splitmix64
//! PRNG seeds ALL choices (which seed doc, how many steps, which verb, which
//! block, which substring, which formatting value). No wall-clock, no thread id:
//! a failing `seed` reproduces bit-for-bit via `repro_one(seed)`. Failures are
//! COLLECTED (not panicked) inside the rayon map so the parallel run reports
//! EVERY failing seed, then the harness asserts the collected failures are empty.
//!
//! TIERS.
//!   * `fuzz_smoke_daily` (#[test], daily): a few hundred deterministic seeds,
//!     inside `just gate`. Catches regressions every run.
//!   * `fuzz_sweep_heavy` (#[ignore], nightly): 20_000 seeds via rayon. Run with
//!     `just -f stemma-engine/Justfile fuzz`.

use rayon::prelude::*;

use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::view::{BlockRole, SegmentView, TextMark, TrackStatus, build_document_view_from_canon};
use stemma::{ExportMode, ExportOptions, ValidatorLevel};

// ─────────────────────────────────────────────────────────────────────────────
// Deterministic PRNG (splitmix64). Pure, seedable, no external crate.
// ─────────────────────────────────────────────────────────────────────────────

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Perturb the raw seed so adjacent case indices diverge immediately.
        Rng(seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0xD1B5_4A32_D192_ED03))
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[0, n)` (n>0).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n as u64)) as usize
    }
    fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Seed-document pool. Built in-memory; deterministic; varied shapes so the
// generator composes verbs over plain text, numbered lists, opaque inlines
// (drawing + field), and a table.
// ─────────────────────────────────────────────────────────────────────────────

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn zip_docx(parts: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, bytes) in parts {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

const CT_BASE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#;
const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOC_RELS_EMPTY: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

fn make_plain_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for p in paragraphs {
        body.push_str(&format!(
            r#"<w:p><w:r><w:t xml:space="preserve">{p}</w:t></w:r></w:p>"#
        ));
    }
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let ct = format!("{CT_BASE}</Types>");
    zip_docx(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", RELS.as_bytes()),
        ("word/_rels/document.xml.rels", DOC_RELS_EMPTY.as_bytes()),
        ("word/document.xml", doc.as_bytes()),
    ])
}

/// A doc carrying a numbering part plus mixed plain/numbered paragraphs.
fn make_numbered_docx() -> Vec<u8> {
    let body = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">First numbered obligation here</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">A plain paragraph between items</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">Second numbered obligation here</w:t></w:r></w:p>"#;
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let numbering = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%2)"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;
    let ct = format!(
        "{CT_BASE}<Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/></Types>"
    );
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;
    zip_docx(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", RELS.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/document.xml", doc.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ])
}

/// A paragraph hosting an inline drawing AND a field opaque (pre-existing
/// anchors the non-shrinking invariant must protect), plus two plain paragraphs.
fn make_opaque_docx() -> Vec<u8> {
    let drawing = r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><wp:extent cx="100" cy="200"/><wp:docPr id="1" name="Picture 1" descr="alt"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:ext cx="9" cy="8"/></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
    let body = format!(
        r#"<w:p><w:r>{drawing}</w:r><w:r><w:t xml:space="preserve">See the Definitions clause for details</w:t></w:r><w:fldSimple w:instr="REF Definitions \h"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p><w:p><w:r><w:t xml:space="preserve">The Confidential Information shall be protected</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Termination requires thirty days written notice</w:t></w:r></w:p>"#
    );
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let ct = format!("{CT_BASE}</Types>");
    zip_docx(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", RELS.as_bytes()),
        ("word/_rels/document.xml.rels", DOC_RELS_EMPTY.as_bytes()),
        ("word/document.xml", doc.as_bytes()),
    ])
}

/// The seed-document pool, built once per case (cheap; parse dominates anyway).
/// Each entry is a distinct structural shape so the generator's verb choices
/// land on plain text, numbered paragraphs, and opaque-bearing paragraphs.
fn seed_doc(which: usize) -> Vec<u8> {
    match which % 5 {
        0 => make_plain_docx(&[
            "The Confidential Information is protected under this Agreement",
            "Each party shall indemnify the other against all claims",
            "This clause survives termination of the Agreement",
        ]),
        1 => make_plain_docx(&[
            "Payment is due within thirty days of the invoice date",
            "Late payments accrue interest at the statutory rate",
        ]),
        2 => make_numbered_docx(),
        3 => make_opaque_docx(),
        _ => make_plain_docx(&[
            "Governing law shall be the laws of the State of Delaware",
            "Any dispute shall be resolved by binding arbitration",
            "The prevailing party may recover reasonable legal fees",
            "Notices must be delivered in writing to the addresses below",
        ]),
    }
}

const SEED_DOC_KINDS: usize = 5;

// ─────────────────────────────────────────────────────────────────────────────
// Engine-independent content fingerprint + opaque inventory (copied verbatim
// from edit_fidelity_invariants.rs — these helpers are test-local there and not
// importable; the contract they encode is identical).
// ─────────────────────────────────────────────────────────────────────────────

fn shape(canon: &CanonDoc) -> String {
    fn status_tag(s: &TrackStatus) -> &'static str {
        match s {
            TrackStatus::Normal => "=",
            TrackStatus::Inserted(_) => "+",
            TrackStatus::Deleted(_) => "-",
            TrackStatus::InsertedThenDeleted { .. } => "±",
        }
    }
    fn marks_tag(marks: &[TextMark]) -> String {
        let mut s = String::new();
        for (m, c) in [
            (TextMark::Bold, 'b'),
            (TextMark::Italic, 'i'),
            (TextMark::Underline, 'u'),
            (TextMark::Strike, 's'),
            (TextMark::Subscript, 'v'),
            (TextMark::Superscript, '^'),
        ] {
            if marks.contains(&m) {
                s.push(c);
            }
        }
        s
    }
    fn role_tag(role: &BlockRole) -> String {
        match role {
            BlockRole::Paragraph => "para".to_string(),
            BlockRole::Heading { level } => format!("h{level}"),
            BlockRole::Table => "table".to_string(),
            BlockRole::Opaque => "opaque".to_string(),
        }
    }
    let view = build_document_view_from_canon(canon);
    let mut out = String::new();
    for (tb, b) in canon.blocks.iter().zip(view.blocks.iter()) {
        let para_fmt = match &tb.block {
            BlockNode::Paragraph(p) => {
                format!(
                    "|align={:?},indent={:?},spacing={:?}",
                    p.align, p.indent, p.spacing
                )
            }
            _ => String::new(),
        };
        out.push_str(&format!(
            "[{}|{}|{}{}]\n",
            role_tag(&b.role),
            b.style_id.as_deref().unwrap_or(""),
            status_tag(&b.block_status),
            para_fmt,
        ));
        for seg in &b.segments {
            match seg {
                SegmentView::Text {
                    text,
                    status,
                    marks,
                    ..
                } => out.push_str(&format!(
                    "  T{}{}:{text}\n",
                    status_tag(status),
                    marks_tag(marks)
                )),
                SegmentView::Opaque { kind, status, .. } => {
                    out.push_str(&format!("  A{}:{kind:?}\n", status_tag(status)))
                }
            }
        }
    }
    out
}

/// `shape()`, restricted to what I1 (reject-all reversibility) actually
/// promises: TRACKED content is restored. A comment is an annotation, not a
/// tracked change (`edit/verbs/comments.rs`'s own contract: "accept-all and
/// reject-all both leave the comment story and its markers intact"), so a
/// `CommentCreate` step in the fuzzed transaction correctly makes
/// `reject_all(tracked)` diverge from `base` on comment-anchor lines alone —
/// that is the annotation contract working, not a fidelity regression.
///
/// Dropping the marker lines alone is not enough: `enumerate_text_spans`
/// correctly terminates a text run AT a comment marker (domain-model §4 — a
/// marker occupies a span position of its own), so a comment landing inside
/// "The prevailing party" splits it into THREE `shape()` lines ("The
/// prevailing ", "party", " may recover..."), where `base` (no comment) has
/// ONE. So after dropping the marker lines, re-coalesce adjacent `T` lines
/// that share the same status+marks prefix — undoing exactly the split three
/// (now-removed) comment markers caused, and nothing else (a real tracked
/// split, e.g. an inserted/deleted word, changes the prefix and is never
/// merged).
///
/// The un-stripped `shape()` remains the fingerprint for I2/I5/I6, where no
/// such carve-out applies (both sides of those comparisons share the same
/// comment markers, or carry none).
fn shape_for_reversibility(canon: &CanonDoc) -> String {
    let comment_marker_lines = ["A=:Comment", "A=:CommentRangeStart", "A=:CommentRangeEnd"];
    let mut out: Vec<String> = Vec::new();
    for line in shape(canon).lines() {
        if comment_marker_lines.contains(&line.trim_start()) {
            continue;
        }
        if let Some(colon) = line.find(':')
            && line.trim_start().starts_with('T')
            && let Some(prev) = out.last_mut()
            && prev.starts_with(&line[..=colon])
        {
            prev.push_str(&line[colon + 1..]);
            continue;
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

fn anchor_ids(doc: &CanonDoc) -> Vec<String> {
    let mut ids = Vec::new();
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline {
                        ids.push(o.id.to_string());
                    }
                }
            }
        }
    }
    ids
}

/// Normalize a plain-text reading to its VISIBLE WORD CONTENT, tolerant of two
/// documented serialize-round-trip representation nuances that are below the
/// content surface (the bytes are correct either way; I3/I3b/I4 already gate
/// structural soundness and anchor preservation):
///
///  1. **Opaque placeholder churn.** A zero-width decoration in the live IR (an
///     inserted bookmark range, a comment range) re-materializes on parse as a
///     counted opaque anchor, so `to_text()` gains one U+FFFC. Stripping U+FFFC
///     compares the readable text, while I4 separately proves the anchor
///     inventory never SHRINKS.
///  2. **Block-vs-mark insertion encoding.** A tracked-inserted paragraph is
///     held block-level in the live IR (reject removes the whole block) but
///     re-encoded as a paragraph-mark + run insertion on parse (reject removes
///     the runs and the mark; a TRAILING inserted paragraph then leaves an empty
///     paragraph shell because there is no following paragraph to merge into —
///     this matches Word's own paragraph-mark-rejection model). Dropping empty
///     lines compares the visible words, not the paragraph-boundary encoding.
///
/// What survives normalization is exactly what must be IDENTICAL: the sequence
/// of visible, non-empty text lines. Real text corruption (a dropped, duplicated,
/// or garbled word) still fails the comparison.
fn normalize_visible(s: &str) -> Vec<String> {
    s.replace('\u{FFFC}', "")
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect()
}

/// The resolution-stable read surface used by I5: a document's style-id sequence
/// (from the accept projection — preserved across a serialize round-trip, unlike
/// the derived heading role) plus its accept-all and reject-all NORMALIZED
/// visible text. Two documents with the same triple read identically under every
/// resolution; this is the semantic surface a serialize round-trip MUST
/// preserve, independent of the internal block-vs-mark tracked-status encoding.
type RoundtripRead = (Vec<Option<String>>, Vec<String>, Vec<String>);

fn roundtrip_read(doc: &Document) -> Result<RoundtripRead, stemma::RuntimeError> {
    let accepted = doc.read_accepted()?;
    let rejected = doc.read_rejected()?;
    let style_ids: Vec<Option<String>> = accepted
        .read()
        .blocks
        .iter()
        .map(|b| b.style_id.clone())
        .collect();
    Ok((
        style_ids,
        normalize_visible(&accepted.to_text()),
        normalize_visible(&rejected.to_text()),
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// A lightweight read of the base CanonDoc the generator needs to target verbs
// validly: per body paragraph, its block id + visible text + whether it carries
// numbering. Built from the public read view (block ids) zipped with the IR
// (numbering / first-word extraction).
// ─────────────────────────────────────────────────────────────────────────────

struct ParaInfo {
    block_id: NodeId,
    text: String,
    numbered: bool,
    /// Whether this paragraph hosts any opaque inline (drawing / field) or a
    /// preserved decoration. A whole-paragraph text replacement on such a
    /// paragraph would destroy the preserved inline — the engine refuses it
    /// (`OpaqueDestroyed`), so the generator must not emit one.
    has_preserved_inline: bool,
    /// The paragraph's block staleness guard (semantic hash), minted from the
    /// base — required by span ops. Valid for the whole transaction because the
    /// generator emits at most one step per paragraph per txn.
    guard: String,
}

fn body_paragraphs(canon: &CanonDoc) -> Vec<ParaInfo> {
    let mut out = Vec::new();
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            let text: String = p
                .segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .filter_map(|i| match i {
                    InlineNode::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            let has_preserved_inline = p.segments.iter().flat_map(|s| s.inlines.iter()).any(|i| {
                matches!(
                    i,
                    InlineNode::OpaqueInline(_)
                        | InlineNode::Decoration(_)
                        | InlineNode::CommentRangeStart { .. }
                        | InlineNode::CommentRangeEnd { .. }
                        | InlineNode::CommentReference { .. }
                )
            });
            out.push(ParaInfo {
                block_id: p.id.clone(),
                text,
                numbered: p.numbering.is_some(),
                has_preserved_inline,
                guard: stemma::semantic_hash::block_semantic_hash_for_block(&tb.block),
            });
        }
    }
    out
}

/// Pick a whole word from `text` (deterministic). Returns `None` when there is
/// no alphabetic word to target (so the generator can choose a different verb).
fn pick_word(rng: &mut Rng, text: &str) -> Option<String> {
    let words: Vec<&str> = text
        .split_whitespace()
        .filter(|w| w.chars().all(|c| c.is_alphabetic()) && w.len() >= 2)
        .collect();
    if words.is_empty() {
        None
    } else {
        Some(rng.pick(&words).to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The verb generator. Produces a vector of VALID `EditStep`s over `base`.
// Validity rules enforced by construction:
//   * every step targets an EXISTING body paragraph by its real block id;
//   * `expect`/word-bearing verbs reference a real substring of that paragraph;
//   * we never stack two text-rewriting verbs on the SAME paragraph in one txn
//     (the second would see stale `expect` text after the first), tracked via
//     `used` set — this keeps the txn internally consistent, which is exactly
//     what a real agent must also do.
// ─────────────────────────────────────────────────────────────────────────────

fn text_content(text: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    }
}

#[derive(Clone, Copy)]
enum Verb {
    ReplaceParagraphText,
    ReplaceSpanWhole,
    SetRunFormatting,
    SetParagraphFormatting,
    ApplyStyle,
    InsertBookmark,
    InsertCrossReference,
    InsertEquationInline,
    CommentCreate,
    InsertNote,
    InsertParagraphAfter,
    SetParagraphNumbering,
}

const ALL_VERBS: &[Verb] = &[
    Verb::ReplaceParagraphText,
    Verb::ReplaceSpanWhole,
    Verb::SetRunFormatting,
    Verb::SetParagraphFormatting,
    Verb::ApplyStyle,
    Verb::InsertBookmark,
    Verb::InsertCrossReference,
    Verb::InsertEquationInline,
    Verb::CommentCreate,
    Verb::InsertNote,
    Verb::InsertParagraphAfter,
    Verb::SetParagraphNumbering,
];

const ALIGNMENTS: &[Alignment] = &[
    Alignment::Left,
    Alignment::Center,
    Alignment::Right,
    Alignment::Justify,
];
const HIGHLIGHTS: &[HighlightColor] = &[
    HighlightColor::Yellow,
    HighlightColor::Green,
    HighlightColor::Cyan,
    HighlightColor::Magenta,
];
const STYLE_IDS: &[&str] = &["Heading1", "Heading2", "Heading3"];
const EQ_M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// Generate one step for `verb` against paragraph `p`. Returns `None` when this
/// paragraph cannot host this verb validly (e.g. no word to bold, or a
/// text-rewrite of a paragraph that already has a pending rewrite); the caller
/// then re-rolls. A step that produces `Some` is, by construction, applicable.
fn gen_step(
    rng: &mut Rng,
    verb: Verb,
    p: &ParaInfo,
    insert_role: Option<&str>,
) -> Option<EditStep> {
    // Whole-paragraph text replacement on a paragraph that hosts a preserved
    // inline (drawing / field / decoration / comment marker) is refused by the
    // engine (`OpaqueDestroyed`) — never emit one.
    if p.has_preserved_inline && matches!(verb, Verb::ReplaceParagraphText | Verb::ReplaceSpanWhole)
    {
        return None;
    }
    match verb {
        Verb::ReplaceParagraphText => Some(EditStep::ReplaceParagraphText {
            block_id: p.block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: p.text.clone(),
            semantic_hash: None,
            content: text_content(&format!("{} (amended)", p.text)),
        }),
        Verb::ReplaceSpanWhole => Some(EditStep::ReplaceSpanText {
            block_id: p.block_id.clone(),
            guard: p.guard.clone(),
            expect: None,
            span: ResolvedSpanSelector::Whole,
            content: text_content(&format!("Revised: {}", p.text)),
            rationale: None,
        }),
        Verb::SetRunFormatting => {
            let word = pick_word(rng, &p.text)?;
            let marks = InlineMarkSet {
                bold: rng.bool(),
                italic: rng.bool(),
                underline: rng.bool(),
                strike: rng.bool(),
                caps: rng.bool(),
                small_caps: rng.bool(),
                ..Default::default()
            };
            // Ensure at least one boolean is set OR a value-bearing style, so the
            // verb is not a no-op (which a real author would not author).
            let style = RunStyleEdit {
                highlight: if rng.bool() {
                    Some(rng.pick(HIGHLIGHTS).clone())
                } else {
                    None
                },
                font_size_half_points: if rng.bool() {
                    Some(20 + (rng.below(8) as u32) * 2)
                } else {
                    None
                },
                ..Default::default()
            };
            if marks.is_empty() && style.is_empty() {
                return None;
            }
            Some(EditStep::SetRunFormatting {
                block_id: p.block_id.clone(),
                expect: word,
                semantic_hash: None,
                marks,
                style,
                rationale: None,
            })
        }
        Verb::SetParagraphFormatting => Some(EditStep::SetParagraphFormatting {
            block_id: p.block_id.clone(),
            semantic_hash: None,
            patch: ParagraphFormattingPatch {
                align: Some(rng.pick(ALIGNMENTS).clone()),
                indent: None,
                spacing: if rng.bool() {
                    Some(ParagraphSpacing {
                        before: Some((rng.below(6) as u32) * 60),
                        after: Some((rng.below(6) as u32) * 60),
                        before_lines: None,
                        after_lines: None,
                        before_autospacing: None,
                        after_autospacing: None,
                        line: None,
                        line_rule: None,
                    })
                } else {
                    None
                },
                borders: None,
                shading: None,
            },
            rationale: None,
        }),
        Verb::ApplyStyle => Some(EditStep::ApplyStyle {
            block_id: p.block_id.clone(),
            semantic_hash: None,
            style_id: rng.pick(STYLE_IDS).to_string(),
            rationale: None,
        }),
        Verb::InsertBookmark => {
            let word = pick_word(rng, &p.text)?;
            Some(EditStep::InsertBookmark {
                block_id: p.block_id.clone(),
                expect: word,
                semantic_hash: None,
                name: format!("Bm{}", rng.below(100000)),
                rationale: None,
            })
        }
        Verb::InsertCrossReference => {
            let word = pick_word(rng, &p.text)?;
            Some(EditStep::InsertCrossReference {
                block_id: p.block_id.clone(),
                expect: word,
                semantic_hash: None,
                spec: RefFieldSpec {
                    kind: RefKind::Ref,
                    bookmark: format!("Target{}", rng.below(1000)),
                    insert_hyperlink: rng.bool(),
                    no_paragraph_number: false,
                    paragraph_number_relative: false,
                    paragraph_number_full: false,
                    suppress_non_delimiter: false,
                    above_below: false,
                    format: FormatSwitches::default(),
                },
                rationale: None,
            })
        }
        Verb::InsertEquationInline => {
            let word = pick_word(rng, &p.text)?;
            let omml = format!(
                r#"<m:oMath xmlns:m="{EQ_M_NS}"><m:r><m:t>a+b={}</m:t></m:r></m:oMath>"#,
                rng.below(10)
            )
            .into_bytes();
            Some(EditStep::InsertEquation {
                block_id: p.block_id.clone(),
                expect: word,
                semantic_hash: None,
                omml,
                placement: EquationPlacement::Inline,
                rationale: None,
            })
        }
        Verb::CommentCreate => {
            let word = pick_word(rng, &p.text)?;
            Some(EditStep::CommentCreate {
                block_id: p.block_id.clone(),
                expect: word,
                semantic_hash: None,
                body: "Fuzz-authored review comment.".to_string(),
                author: Some("Fuzz".to_string()),
                rationale: None,
            })
        }
        Verb::InsertNote => {
            let word = pick_word(rng, &p.text)?;
            Some(EditStep::InsertNote {
                block_id: p.block_id.clone(),
                expect: word,
                semantic_hash: None,
                note_kind: if rng.bool() {
                    NoteKind::Footnote
                } else {
                    NoteKind::Endnote
                },
                body: "See the attached schedule.".to_string(),
                rationale: None,
            })
        }
        Verb::InsertParagraphAfter => {
            // Only when the document offers a stable (non-numbering) body role.
            let role = insert_role?;
            Some(EditStep::InsertParagraphs {
                anchor_block_id: p.block_id.clone(),
                position: InsertPosition::After,
                rationale: None,
                blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some(role.to_string()),
                    content: text_content(&format!("Inserted clause {}", rng.below(100000))),
                    restart_numbering: false,
                    list: None,
                })],
            })
        }
        Verb::SetParagraphNumbering => {
            // Only meaningful when the seed doc carries a numbering part. We
            // model two transitions: attach a level / set a level on a numbered
            // para, detach a numbered para. For a plain para in a doc with no
            // numbering part, attach would dangle — so only emit on a numbered
            // paragraph (reachable in the numbered seed doc).
            if !p.numbered {
                return None;
            }
            let change = if rng.bool() {
                NumberingChange::Remove
            } else {
                NumberingChange::SetLevel {
                    ilvl: 1,
                    synthesized_text: "(a)".to_string(),
                    is_bullet: false,
                }
            };
            Some(EditStep::SetParagraphNumbering {
                block_id: p.block_id.clone(),
                semantic_hash: None,
                change,
                rationale: None,
            })
        }
    }
}

/// Generate a whole transaction's steps for `base`. 1..=4 steps; each step
/// targets a paragraph, with at most one TEXT-REWRITE (ReplaceParagraphText /
/// ReplaceSpanWhole / SetParagraphNumbering-on-this-para) per paragraph per txn
/// to keep `expect` preconditions internally consistent across the batch.
fn gen_steps(rng: &mut Rng, base: &CanonDoc) -> Vec<EditStep> {
    let paras = body_paragraphs(base);
    if paras.is_empty() {
        return Vec::new();
    }
    // The role an `InsertParagraphAfter` uses must EXIST in the document's
    // vocabulary AT THE STEP that resolves it — and the vocabulary is EMERGENT:
    // it is recomputed from the live paragraphs' formatting/numbering signatures
    // on every step. Many verbs perturb it (ApplyStyle swaps a role;
    // SetParagraphFormatting changes a paragraph's pPr signature and reclassifies
    // it; SetParagraphNumbering attaches/detaches a numbered role). A non-numbered
    // body role derived from `base` is therefore NOT guaranteed to survive a
    // sibling step in the same transaction.
    //
    // Rather than chase every interaction, we partition transactions into two
    // honest kinds (a real agent faces the same constraint):
    //   * INSERT transactions: a single `InsertParagraphAfter`, anchored on a
    //     non-numbered paragraph and using its own (stable, untouched) body role;
    //   * COMPOSITION transactions: 1..=4 steps drawn from every OTHER verb, at
    //     most one per paragraph, which never depend on an emergent role id.
    // Both still exercise the full fidelity invariant set on every seed shape.
    let vocab = stemma::vocabulary::extract_vocabulary(base);
    let body_role: Option<String> = vocab
        .paragraph_roles
        .iter()
        .find(|r| !r.has_numbering)
        .map(|r| r.id.clone());

    // 1-in-5 transactions are a solo paragraph insert (when a stable body role
    // and a non-numbered anchor both exist).
    if rng.below(5) == 0
        && let Some(role) = &body_role
        && let Some(anchor) = paras.iter().find(|p| !p.numbered)
    {
        return vec![EditStep::InsertParagraphs {
            anchor_block_id: anchor.block_id.clone(),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some(role.clone()),
                content: text_content(&format!("Inserted clause {}", rng.below(100000))),
                restart_numbering: false,
                list: None,
            })],
        }];
    }

    // COMPOSITION transaction. At most ONE step per paragraph (the engine refuses
    // to stack two tracked changes on one paragraph within a txn, and a later
    // expect-bearing verb would see stale text after a prior rewrite). No
    // `InsertParagraphAfter` here — it lives only in the solo insert branch.
    let n_steps = 1 + rng.below(4);
    let mut steps = Vec::new();
    let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut attempts = 0;
    while steps.len() < n_steps && attempts < n_steps * 8 {
        attempts += 1;
        let p = &paras[rng.below(paras.len())];
        if touched.contains(&p.block_id.to_string()) {
            continue;
        }
        let verb = *rng.pick(ALL_VERBS);
        if matches!(verb, Verb::InsertParagraphAfter) {
            continue;
        }
        if let Some(step) = gen_step(rng, verb, p, None) {
            touched.insert(p.block_id.to_string());
            steps.push(step);
        }
    }
    steps
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Fuzz".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The per-case invariant harness. Returns `Ok(assertions_run)` or `Err(reason)`
// — a STRUCTURED failure carrying the seed for repro (no panic, so the parallel
// run can report every failing seed).
// ─────────────────────────────────────────────────────────────────────────────

struct CaseOutcome {
    /// Number of invariant assertions actually checked (for coverage reporting).
    assertions: u64,
}

fn run_case(seed: u64) -> Result<CaseOutcome, String> {
    let mut rng = Rng::new(seed);
    let which = rng.below(SEED_DOC_KINDS);
    let bytes = seed_doc(which);

    let base_doc =
        Document::parse(&bytes).map_err(|e| format!("seed {seed}: base parse failed: {e:?}"))?;
    let base = base_doc.snapshot().canonical.clone();

    let steps = gen_steps(&mut rng, &base);
    if steps.is_empty() {
        // Degenerate (no body paragraphs) — not a fidelity case. Count nothing.
        return Ok(CaseOutcome { assertions: 0 });
    }
    let n_steps = steps.len();

    // ── Apply TRACKED. A generation bug (an invalid step) surfaces here as an
    // apply error; we treat that as a HARNESS failure (the generator must only
    // emit valid steps), not a fidelity finding, and report the seed.
    let tracked_doc = base_doc
        .apply(&txn(steps.clone(), MaterializationMode::TrackedChange))
        .map_err(|e| {
            format!("seed {seed} (doc kind {which}, {n_steps} steps): tracked apply failed: {e:?}")
        })?;
    let tracked = tracked_doc.snapshot().canonical.clone();

    let mut assertions: u64 = 0;

    // ── I1: reject_all(tracked) ≡ base, modulo comment-anchor markers (see
    // `shape_for_reversibility`: a comment is an annotation, not a tracked
    // change, so it correctly survives reject-all even though nothing else
    // does).
    let rejected = tracked_doc
        .project(stemma::Resolution::RejectAll)
        .map_err(|e| format!("seed {seed}: reject projection failed: {e:?}"))?;
    assertions += 1;
    if shape_for_reversibility(&base) != shape_for_reversibility(&rejected.snapshot().canonical) {
        return Err(format!(
            "seed {seed} (doc {which}, {n_steps} steps): I1 reject-all != baseline\n--- base ---\n{}\n--- rejected ---\n{}",
            shape(&base),
            shape(&rejected.snapshot().canonical)
        ));
    }

    // ── I2: accept_all(tracked) ≡ direct apply.
    let accepted = tracked_doc
        .project(stemma::Resolution::AcceptAll)
        .map_err(|e| format!("seed {seed}: accept projection failed: {e:?}"))?;
    let direct = base_doc
        .apply(&txn(steps.clone(), MaterializationMode::Direct))
        .map_err(|e| format!("seed {seed}: direct apply failed: {e:?}"))?;
    assertions += 1;
    if shape(&accepted.snapshot().canonical) != shape(&direct.snapshot().canonical) {
        return Err(format!(
            "seed {seed} (doc {which}, {n_steps} steps): I2 accept-all != direct\n--- accepted ---\n{}\n--- direct ---\n{}",
            shape(&accepted.snapshot().canonical),
            shape(&direct.snapshot().canonical)
        ));
    }

    // ── I3: serialize redline under the BLOCKING validator => zero blocking
    // findings (serialize returns Err(ValidationFailed) otherwise).
    let redline_bytes = tracked_doc
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .map_err(|e| {
            format!("seed {seed} (doc {which}, {n_steps} steps): I3 blocking validator rejected serialized redline: {e:?}")
        })?;
    assertions += 1;

    // ── I3b: independent re-validation of the emitted bytes (defence in depth:
    // `stemma::api::validate` re-runs the package + ooxml checks as a property
    // of the bytes, not of the snapshot).
    let report = stemma::api::validate(&redline_bytes);
    assertions += 1;
    if !report.ok {
        return Err(format!(
            "seed {seed} (doc {which}, {n_steps} steps): I3b validate(bytes).ok == false: {:?}",
            report.issues
        ));
    }

    // ── I4: non-shrinking opaque inventory (every pre-edit anchor survives).
    let before = anchor_ids(&base);
    let after = anchor_ids(&tracked);
    assertions += 1;
    for id in &before {
        if !after.contains(id) {
            return Err(format!(
                "seed {seed} (doc {which}, {n_steps} steps): I4 opaque anchor '{id}' dropped by the edit"
            ));
        }
    }

    // ── I5: parse(serialize(redline)) round-trips at the SEMANTIC surface.
    //
    // The honest round-trip property is resolution-stable: the emitted bytes,
    // re-decoded, must read the SAME under each resolution as the in-memory
    // tracked document — and reject-all of the re-decoded redline must still
    // reconstruct the base. We deliberately do NOT require the raw tracked
    // `shape()` to byte-equal the reparsed `shape()`: a tracked-INSERTED
    // paragraph is legitimately re-encoded with its insertion at the
    // paragraph-mark/run level rather than a block-level "+" status, and the
    // derived `heading_level` cache (role) is recomputed from `style_id` on
    // parse but not refreshed by `ApplyStyle`. Both are below the semantic
    // surface — the bytes are correct (and the BLOCKING validator + I3 already
    // proved structural soundness); over-specifying I5 to the internal status
    // encoding would encode an implementation detail, not the domain rule.
    let reparsed = Document::parse(&redline_bytes).map_err(|e| {
        format!("seed {seed} (doc {which}, {n_steps} steps): I5 reparse of redline failed: {e:?}")
    })?;

    // The style-id sequence (preserved across the round-trip, unlike the derived
    // heading role) plus the per-resolution reading is the surface that must
    // survive. `roundtrip_read(doc)` => (style_ids, accept_text, reject_text).
    let read_tracked = roundtrip_read(&tracked_doc)
        .map_err(|e| format!("seed {seed}: tracked projection failed: {e:?}"))?;
    let read_reparsed = roundtrip_read(&reparsed)
        .map_err(|e| format!("seed {seed}: reparsed projection failed: {e:?}"))?;

    assertions += 1;
    if read_tracked != read_reparsed {
        return Err(format!(
            "seed {seed} (doc {which}, {n_steps} steps): I5 parse(serialize(redline)) reads differently than the tracked IR\n--- tracked (styles | accept | reject) ---\n{read_tracked:?}\n--- reparsed ---\n{read_reparsed:?}"
        ));
    }

    // I5b: reject-all of the RE-DECODED redline still reconstructs the base text
    // (round-trip reversibility — the strongest single fidelity statement: the
    // bytes a counterparty would open and reject return to where we started).
    assertions += 1;
    let reparsed_reject = normalize_visible(
        &reparsed
            .read_rejected()
            .map_err(|e| format!("seed {seed}: reparsed reject failed: {e:?}"))?
            .to_text(),
    );
    let base_reject = normalize_visible(
        &base_doc
            .read_rejected()
            .map_err(|e| format!("seed {seed}: base reject failed: {e:?}"))?
            .to_text(),
    );
    if reparsed_reject != base_reject {
        return Err(format!(
            "seed {seed} (doc {which}, {n_steps} steps): I5b reject-all of the re-decoded redline != base text\n--- reparsed-reject ---\n{reparsed_reject:?}\n--- base ---\n{base_reject:?}"
        ));
    }

    // ── I6: atomicity. Append a guaranteed-stale step (a ReplaceParagraphText
    // whose `expect` cannot match) to the SAME valid steps; the whole
    // transaction must fail AND leave the base untouched (apply returns a fresh
    // Document; base_doc is never mutated — we re-read it to prove it).
    let target_para = body_paragraphs(&base)
        .into_iter()
        .next()
        .expect("seed doc has >=1 paragraph (steps non-empty implies paras non-empty)");
    let mut poisoned = steps.clone();
    poisoned.push(EditStep::ReplaceParagraphText {
        block_id: target_para.block_id.clone(),
        rationale: None,
        replacement_role: None,
        // A string that cannot be a substring of any real paragraph.
        expect: "\u{0}STALE-EXPECT-NEVER-MATCHES\u{0}".to_string(),
        semantic_hash: None,
        content: text_content("should never apply"),
    });
    let atomic_result = base_doc.apply(&txn(poisoned, MaterializationMode::TrackedChange));
    assertions += 1;
    if atomic_result.is_ok() {
        return Err(format!(
            "seed {seed} (doc {which}, {n_steps} steps): I6 a transaction ending in a stale step must FAIL, but it succeeded"
        ));
    }
    // base_doc is a value; the failed apply produced no new Document, and the
    // original is unchanged by construction. Assert its shape still equals the
    // pristine base (the rollback is total).
    assertions += 1;
    if shape(&base_doc.snapshot().canonical) != shape(&base) {
        return Err(format!(
            "seed {seed} (doc {which}, {n_steps} steps): I6 base mutated by a failed atomic transaction"
        ));
    }

    Ok(CaseOutcome { assertions })
}

/// Reproduce + (re)assert a single seed, panicking with the structured reason.
/// The entry point a failing seed from the sweep is debugged through.
fn repro_one(seed: u64) {
    match run_case(seed) {
        Ok(_) => {}
        Err(reason) => panic!("{reason}"),
    }
}

/// Run `seeds` in parallel; return (cases_run, total_assertions, failures).
fn run_sweep(seeds: std::ops::Range<u64>) -> (u64, u64, Vec<String>) {
    let results: Vec<Result<u64, String>> = seeds
        .into_par_iter()
        .map(|seed| run_case(seed).map(|o| o.assertions))
        .collect();
    let mut cases = 0u64;
    let mut assertions = 0u64;
    let mut failures = Vec::new();
    for r in results {
        cases += 1;
        match r {
            Ok(a) => assertions += a,
            Err(reason) => failures.push(reason),
        }
    }
    (cases, assertions, failures)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tiers.
// ─────────────────────────────────────────────────────────────────────────────

/// DAILY smoke: a few hundred deterministic seeds inside `just gate`. Fast
/// (well under a second on the in-memory fixtures) and fully reproducible — a
/// regression in any verb's reversibility / accept==direct / serialize-soundness
/// surfaces here every run, with the failing seed in the message.
#[test]
fn fuzz_smoke_daily() {
    let (cases, assertions, failures) = run_sweep(0..400);
    assert!(
        failures.is_empty(),
        "fuzz smoke: {} of {cases} cases violated a fidelity invariant ({assertions} assertions ran). \
         Reproduce a failing seed with `fuzz_repro_seed` (edit the SEED const) or read below:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    // Coverage floor: the smoke tier must actually exercise the invariants, not
    // silently degenerate to zero work.
    assert!(
        assertions > 1500,
        "smoke tier ran too few assertions: {assertions}"
    );
}

/// NIGHTLY heavy sweep: 20_000 seeds, rayon-parallel. Run with
/// `just -f stemma-engine/Justfile fuzz`. `#[ignore]` keeps it out of the daily gate
/// (it is the expensive tier), per the testing-strategy carve-out.
#[test]
#[ignore = "heavy fuzz sweep (20k cases) — nightly tier; run via `just -f stemma-engine/Justfile fuzz`"]
fn fuzz_sweep_heavy() {
    let (cases, assertions, failures) = run_sweep(0..20_000);
    eprintln!(
        "fuzz sweep: {cases} cases, {assertions} invariant assertions, {} failures",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "fuzz sweep: {} of {cases} cases violated a fidelity invariant. Failing seeds (reproduce via fuzz_repro_seed):\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Repro harness: pin a single seed here to debug a failure under a normal
/// (non-parallel) test with a backtrace. Default seed 0 must always pass.
#[test]
fn fuzz_repro_seed() {
    const SEED: u64 = 0;
    repro_one(SEED);
}
