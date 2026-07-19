//! `plan_find_replace_all` — a **pure planner** that composes existing
//! `EditStep::ReplaceParagraphText` steps to perform a tracked find-and-replace
//! across a document. It adds NO new `EditStep`, NO new wire `Op`, and never
//! touches the materializer (Invariant M): every edit it emits rides the exact
//! `ReplaceParagraphText` path the v3/v4 callers already use, so accept/reject
//! and opaque preservation are handled centrally.
//!
//! # Model
//!
//! A paragraph's visible content is a sequence of **text sections** separated by
//! **barrier anchors** — opaque inlines (images, fields, hyperlinks, footnote
//! refs, …) and hard breaks. This is exactly what `extract_text_sections`
//! returns: for `N` anchors there are `N + 1` sections. `ReplaceParagraphText`
//! matches its `expect` *section-locally* (the substring must lie inside one
//! section, not span an anchor), and it rewrites the whole paragraph by
//! interleaving new text with a `PreservedInlineRef` for every anchor in
//! original order.
//!
//! So the planner's job per paragraph is:
//!   1. Flatten the paragraph into `[Section(text), Anchor(id), Section(text), …]`
//!      in document order — the same order `collect_anchor_inventory` /
//!      `extract_text_sections` see.
//!   2. Replace every in-section occurrence of `needle` with `replacement`.
//!   3. Build ONE `ReplaceParagraphText` whose `ParagraphContent` interleaves the
//!      rewritten section text (as `Text` fragments) with a `PreservedInlineRef`
//!      for each anchor, in order.
//!   4. Derive `expect` from the FIRST section that actually changed — the whole
//!      original section text — so the precondition is guaranteed section-local
//!      and present (the same notion of "section" `ReplaceParagraphText` uses).
//!
//! # Barriers (no silent fallback)
//!
//! A needle that would straddle a barrier anchor never matches inside a single
//! section, so section-local matching simply skips it. That is correct but
//! invisible, which violates "no silent fallbacks". We therefore *detect* the
//! straddle explicitly: a needle present in the paragraph's full visible text
//! but absent from every section is a **barrier straddle**. The policy decides:
//!   - `BarrierPolicy::Skip` — leave the paragraph untouched (visible: the step
//!     is simply not emitted; documented here and test-covered).
//!   - `BarrierPolicy::Fail` — fail the whole plan with
//!     `EditError::FindReplaceBarrierStraddle` carrying the block id.
//!
//! Never half-edit across a barrier.
//!
//! # Already-tracked segments (no silent history fold)
//!
//! `apply_transaction`'s `ReplaceParagraphText` path calls
//! `prepare_paragraph_for_direct_edit`, which auto-accepts any existing tracked
//! segments before diffing. For an *author-initiated* find-replace that would
//! silently fold unrelated revision history into the new edit. The planner
//! refuses up front: a paragraph whose target sections carry any non-`Normal`
//! segment yields `EditError::ParagraphContainsTrackedSegments` instead of an
//! emitted step. (The block must also be `Normal`, else
//! `EditError::BlockHasTrackedStatus`.)
//!
//! # Matching rules
//!
//! - `case_sensitive == false`: matching folds ASCII case, but the **literal
//!   `replacement` casing is always written** (we never reconstruct the matched
//!   casing). Documented contract: "replace `color`/`Color`/`COLOR` with
//!   `colour`" writes `colour` every time.
//! - `whole_word == true`: a match counts only when both boundaries are a
//!   Unicode non-alphanumeric char (or string edge). "cat" does not match inside
//!   "category"; it does match in "the cat sat" and "(cat)".
//!
//! # Scope
//!
//! v1 ships `FindReplaceScope::BodyOnly` (top-level body paragraphs). The
//! `BodyAndStories` variant is wired through `story_addr` for footnote/endnote/
//! comment stories but is gated as opt-in and documented as best-tested on the
//! body path; it walks the same per-paragraph planner over story blocks.

use super::super::{
    ContentFragment, EditError, EditStep, ParagraphContent, block_at, find_paragraph_path,
};
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, ParagraphNode, TrackedBlock, TrackingStatus,
};
use crate::semantic_hash::block_semantic_hash_for_block;

/// What to replace, where, and how. Built at the edge (MCP/CLI) and handed to
/// the pure planner — a validated intent carrier, not a new IR shape.
#[derive(Clone, Debug)]
pub struct FindReplaceOptions {
    /// The literal substring to find. Empty needle => no-op (empty plan).
    pub needle: String,
    /// The literal replacement text. Written verbatim, including its casing,
    /// even under case-insensitive matching.
    pub replacement: String,
    /// Which stories to search.
    pub scope: FindReplaceScope,
    /// Whether matching is case-sensitive.
    pub case_sensitive: bool,
    /// Whether matches must be whole words (Unicode non-alphanumeric boundary).
    pub whole_word: bool,
    /// What to do when a needle straddles a barrier anchor.
    pub on_barrier_match: BarrierPolicy,
}

/// Which document stories the find-replace walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindReplaceScope {
    /// Top-level body paragraphs only (v1 must-ship).
    BodyOnly,
    /// Body plus footnote/endnote/comment story paragraphs (opt-in).
    BodyAndStories,
}

/// What to do when a needle would straddle a barrier anchor (opaque inline,
/// field, hyperlink, hard break) — never half-edit across it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierPolicy {
    /// Leave the straddling paragraph untouched (no step emitted).
    Skip,
    /// Fail the whole plan loudly.
    Fail,
}

/// One element of a flattened paragraph: a run of visible text (a "section"),
/// or a barrier anchor referenced by id.
enum Element {
    Section(String),
    Anchor(NodeId),
}

/// Plan a tracked find-and-replace across the document. Returns the
/// `EditStep`s (one `ReplaceParagraphText` per changed paragraph) to wrap in an
/// `EditTransaction`. An empty `Vec` means nothing matched (or `needle ==
/// replacement`, or `needle` is empty): a true no-op, zero tracked spans.
///
/// Fails loudly (no partial plan) on the first paragraph that:
///   - has a tracked block status (`BlockHasTrackedStatus`),
///   - contains existing tracked segments (`ParagraphContainsTrackedSegments`),
///   - or straddles a barrier under `BarrierPolicy::Fail`
///     (`FindReplaceBarrierStraddle`).
pub fn plan_find_replace_all(
    doc: &CanonDoc,
    opts: &FindReplaceOptions,
) -> Result<Vec<EditStep>, EditError> {
    // Identity / empty needle => no-op. We refuse to author zero-content edits.
    if opts.needle.is_empty() || opts.needle == opts.replacement {
        return Ok(Vec::new());
    }

    let mut steps = Vec::new();

    // Body paragraphs. `step_index` mirrors the position in the emitted plan so
    // error context lines up with the transaction that will carry these steps.
    for tracked in &doc.blocks {
        if let BlockNode::Paragraph(para) = &tracked.block {
            let step_index = steps.len();
            if let Some(step) = plan_paragraph(para, &tracked.status, opts, step_index)? {
                steps.push(step);
            }
        }
    }

    if opts.scope == FindReplaceScope::BodyAndStories {
        // Opt-in: walk footnote/endnote/comment story paragraphs with the SAME
        // per-paragraph planner. Story paragraphs carry their own block status
        // and segments, so the same Normal-only gate applies. (Body is the
        // must-ship / best-tested path; stories ride the identical logic.)
        for story in &doc.footnotes {
            plan_story_blocks(&story.blocks, opts, &mut steps)?;
        }
        for story in &doc.endnotes {
            plan_story_blocks(&story.blocks, opts, &mut steps)?;
        }
        for story in &doc.comments {
            plan_story_blocks(&story.blocks, opts, &mut steps)?;
        }
    }

    Ok(steps)
}

/// Plan over a story's block list (footnote/endnote/comment), appending steps.
fn plan_story_blocks(
    blocks: &[TrackedBlock],
    opts: &FindReplaceOptions,
    steps: &mut Vec<EditStep>,
) -> Result<(), EditError> {
    for tracked in blocks {
        if let BlockNode::Paragraph(para) = &tracked.block {
            let step_index = steps.len();
            if let Some(step) = plan_paragraph(para, &tracked.status, opts, step_index)? {
                steps.push(step);
            }
        }
    }
    Ok(())
}

/// Plan a single paragraph. Returns `None` when the paragraph needs no edit
/// (no match, or a skipped barrier straddle). Returns an error for a tracked
/// block/segment or a barrier straddle under `Fail`.
fn plan_paragraph(
    para: &ParagraphNode,
    block_status: &TrackingStatus,
    opts: &FindReplaceOptions,
    step_index: usize,
) -> Result<Option<EditStep>, EditError> {
    let block_id = para.id.clone();

    // Flatten into ordered sections + anchors.
    let elements = flatten_paragraph(para);
    let sections: Vec<&String> = elements
        .iter()
        .filter_map(|e| match e {
            Element::Section(s) => Some(s),
            Element::Anchor(_) => None,
        })
        .collect();

    // Does the needle occur in any single section? (the only matchable case)
    let any_section_match = sections.iter().any(|s| find_match(s, opts).is_some());

    if !any_section_match {
        // No section-local match. But does it straddle a barrier? A needle
        // present in the full visible text yet absent from every section is a
        // straddle — surface it, never silently skip without a policy decision.
        let visible = para_visible_text(&elements);
        let straddles = find_match(&visible, opts).is_some();
        if straddles {
            match opts.on_barrier_match {
                BarrierPolicy::Skip => return Ok(None),
                BarrierPolicy::Fail => {
                    return Err(EditError::FindReplaceBarrierStraddle {
                        block_id,
                        needle: opts.needle.clone(),
                        step_index,
                    });
                }
            }
        }
        return Ok(None);
    }

    // There IS an editable match. Refuse to fold unrelated tracked history:
    // the block and all segments must be Normal (the same gate
    // `validate_replace_step` enforces, asserted here so we never emit a step
    // that would silently auto-accept history via prepare_paragraph_for_direct_edit).
    match block_status {
        TrackingStatus::Normal => {}
        TrackingStatus::Inserted(_) => {
            return Err(EditError::BlockHasTrackedStatus {
                block_id,
                status: "inserted",
                step_index,
            });
        }
        TrackingStatus::Deleted(_) => {
            return Err(EditError::BlockHasTrackedStatus {
                block_id,
                status: "deleted",
                step_index,
            });
        }
        TrackingStatus::InsertedThenDeleted(_) => {
            return Err(EditError::BlockHasTrackedStatus {
                block_id,
                status: "inserted_then_deleted",
                step_index,
            });
        }
    }
    for segment in &para.segments {
        if segment.status != TrackingStatus::Normal {
            return Err(EditError::ParagraphContainsTrackedSegments {
                block_id,
                step_index,
            });
        }
    }

    // Build the rewritten content and capture the first changed section's
    // ORIGINAL text as the `expect` precondition (guaranteed section-local).
    let mut fragments: Vec<ContentFragment> = Vec::new();
    let mut expect: Option<String> = None;
    for element in &elements {
        match element {
            Element::Section(original) => {
                let rewritten = replace_in_section(original, opts);
                if expect.is_none() && rewritten != *original {
                    expect = Some(original.clone());
                }
                // Always emit the (possibly unchanged) section text so the
                // whole-paragraph content round-trips every section faithfully.
                fragments.push(ContentFragment::Text(rewritten));
            }
            Element::Anchor(id) => {
                fragments.push(ContentFragment::PreservedInlineRef(id.clone()));
            }
        }
    }

    let expect = expect.expect(
        "any_section_match guarantees at least one section changed under replace_in_section",
    );

    let semantic_hash = block_semantic_hash_for_block(&BlockNode::from(para.clone()));

    Ok(Some(EditStep::ReplaceParagraphText {
        block_id,
        rationale: None,
        replacement_role: None,
        expect,
        semantic_hash: Some(semantic_hash),
        content: ParagraphContent { fragments },
    }))
}

/// Flatten a paragraph into ordered `Section`/`Anchor` elements, mirroring the
/// section model of `extract_text_sections` + `collect_anchor_inventory`. Text
/// runs between anchors are concatenated into one section; each opaque inline
/// or hard break is an anchor boundary. Zero-width nodes (decorations, comment
/// markers) are skipped (they do not affect text positions or sections).
fn flatten_paragraph(para: &ParagraphNode) -> Vec<Element> {
    let mut elements = Vec::new();
    let mut current = String::new();
    for segment in &para.segments {
        for inline in &segment.inlines {
            match inline {
                InlineNode::Text(t) => current.push_str(&t.text),
                InlineNode::OpaqueInline(o) => {
                    elements.push(Element::Section(std::mem::take(&mut current)));
                    elements.push(Element::Anchor(o.id.clone()));
                }
                InlineNode::HardBreak(hb) => {
                    elements.push(Element::Section(std::mem::take(&mut current)));
                    elements.push(Element::Anchor(hb.id.clone()));
                }
                // Zero-width: decorations and comment markers do not split sections.
                _ => {}
            }
        }
    }
    elements.push(Element::Section(current));
    elements
}

/// The paragraph's full visible text (anchors contribute nothing), used only to
/// detect barrier straddles.
fn para_visible_text(elements: &[Element]) -> String {
    let mut out = String::new();
    for e in elements {
        if let Element::Section(s) = e {
            out.push_str(s);
        }
    }
    out
}

/// Replace every (non-overlapping, left-to-right) match of the needle in one
/// section with the literal replacement. Honors case-sensitivity and
/// whole-word boundaries. The literal `replacement` casing is always written.
fn replace_in_section(section: &str, opts: &FindReplaceOptions) -> String {
    let chars: Vec<char> = section.chars().collect();
    let needle: Vec<char> = opts.needle.chars().collect();
    if needle.is_empty() {
        return section.to_string();
    }

    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if matches_at(&chars, i, &needle, opts) {
            out.push_str(&opts.replacement);
            i += needle.len();
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Find the first match offset of the needle in `haystack` under the options,
/// or `None`. Used for the "is there any match" / "does it straddle" queries.
fn find_match(haystack: &str, opts: &FindReplaceOptions) -> Option<usize> {
    let chars: Vec<char> = haystack.chars().collect();
    let needle: Vec<char> = opts.needle.chars().collect();
    if needle.is_empty() {
        return None;
    }
    let mut i = 0usize;
    while i + needle.len() <= chars.len() {
        if matches_at(&chars, i, &needle, opts) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Does `needle` match `chars` starting at index `i`, under case + whole-word
/// rules? `whole_word` requires a Unicode non-alphanumeric boundary (or string
/// edge) on both sides.
fn matches_at(chars: &[char], i: usize, needle: &[char], opts: &FindReplaceOptions) -> bool {
    if i + needle.len() > chars.len() {
        return false;
    }
    for (k, nc) in needle.iter().enumerate() {
        let hc = chars[i + k];
        let eq = if opts.case_sensitive {
            hc == *nc
        } else {
            chars_eq_ignore_case(hc, *nc)
        };
        if !eq {
            return false;
        }
    }
    if opts.whole_word {
        let before_ok = i == 0 || !chars[i - 1].is_alphanumeric();
        let after_idx = i + needle.len();
        let after_ok = after_idx == chars.len() || !chars[after_idx].is_alphanumeric();
        if !(before_ok && after_ok) {
            return false;
        }
    }
    true
}

/// Case-insensitive char equality. Folds both sides to lowercase; a char that
/// lowercases to multiple chars (rare) compares by its full folded sequence.
fn chars_eq_ignore_case(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    a.to_lowercase().eq(b.to_lowercase())
}

// ═══════════════════════════════════════════════════════════════════════════
// replace_text — tracked-native find/replace that SPLICES through tracked
// paragraphs.
//
// Where `plan_find_replace_all` builds one whole-paragraph `ReplaceParagraphText`
// and REFUSES paragraphs that already carry tracked changes (its path
// auto-accepts history before diffing — a silent fold), `plan_replace_text`
// builds one status-preserving SPLICE (`EditStep::ReplaceSpanText`) over the
// visible pending region a match lands in. Normal text and pending insertions
// are editable; deletions and terminal inserted-then-deleted segments remain
// walls. This is the headline tracked-native verb: "find this phrase even
// inside a redlined paragraph, replace it as a tracked change."
//
// Model: a paragraph is a sequence of editable regions (maximal runs sharing
// one Normal/Inserted status) separated by walls — opaque anchors, hard breaks,
// deletions, and terminal stacked segments. Matching happens inside a single region. One paragraph yields AT
// MOST ONE splice (the single-edit-per-paragraph rule: two splices would
// collide on the block guard); when a paragraph has matches in more than one
// region the planner targets the minimal flat-inline range covering them, which
// must not contain a tracked segment (else it is refused per the barrier
// policy). The splice's internal text diff localizes multiple matches within the
// targeted range, so the planner never needs character offsets.
// ═══════════════════════════════════════════════════════════════════════════

use super::super::{ResolvedSpanEndpoint, ResolvedSpanSelector};

/// Which occurrences `replace_text` must touch. Default is exactly one (the
/// common "this specific phrase" case); `All` replaces everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedMatches {
    Count(usize),
    All,
}

/// Equivalence used when matching `old` against the document text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    /// Byte-exact (default).
    Exact,
    /// Fold visually-equivalent characters before comparing — see
    /// [`NormalizationClass`]. The replacement is still written verbatim and
    /// untouched bytes stay byte-identical; only the COMPARISON is normalized.
    NormalizeWs,
}

/// A class of visual-equivalence folding applied under [`MatchMode::NormalizeWs`].
/// The receipt reports exactly which classes ACTUALLY fired on a match (an
/// equivalence the caller relied on is visible, not discovered through a later
/// surprise). The classes (documented contract):
///   - `Whitespace`: the NBSP family + typographic spaces + tab fold to ASCII
///     space, 1:1 (NO run-collapsing — one needle space matches one whitespace
///     char). Members: U+00A0, U+202F, U+2007, U+2002..=U+200A, U+0009 → U+0020.
///   - `Apostrophe`: U+2019 (’) and U+02BC fold to U+0027 (').
///   - `DoubleQuote`: U+201C U+201D (“ ”) fold to U+0022 (").
///   - `SingleQuote`: U+2018 U+2019 (‘ ’) fold to U+0027 (').
///
/// Dashes, ellipsis, and line breaks are deliberately NOT folded in v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizationClass {
    Whitespace,
    Apostrophe,
    DoubleQuote,
    SingleQuote,
}

impl NormalizationClass {
    /// Stable wire string for the receipt.
    pub fn as_str(self) -> &'static str {
        match self {
            NormalizationClass::Whitespace => "whitespace",
            NormalizationClass::Apostrophe => "apostrophe",
            NormalizationClass::DoubleQuote => "double_quote",
            NormalizationClass::SingleQuote => "single_quote",
        }
    }
}

/// Which blocks `replace_text` searches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplaceTextScope {
    /// Every top-level body paragraph (the frozen CLI worklist coverage).
    WholeDoc,
    /// Every body paragraph, including paragraphs nested in table cells.
    BodyAndTables,
    /// The inclusive top-level body slice `[from..=to]` in document order.
    Range { from: NodeId, to: NodeId },
    /// One paragraph, either top-level or nested in a table cell.
    SingleBlock(NodeId),
}

/// `replace_text` intent, built at the edge and handed to the pure planner.
#[derive(Clone, Debug)]
pub struct ReplaceTextOptions {
    /// The literal phrase to find (compared under `match_mode`).
    pub old: String,
    /// The literal replacement, written verbatim.
    pub new: String,
    /// Author stamped on the resulting tracked change.
    pub author: String,
    pub scope: ReplaceTextScope,
    pub expected: ExpectedMatches,
    pub match_mode: MatchMode,
    /// What to do when a match would straddle a wall (anchor or tracked segment).
    pub on_barrier_match: BarrierPolicy,
}

/// One replaced occurrence, for the census, the count gate's error excerpts, and
/// the receipt.
#[derive(Clone, Debug)]
pub struct MatchSite {
    pub block_id: NodeId,
    /// Capped surrounding context with the match delimited, e.g. `…the «old» x…`.
    pub excerpt: String,
}

/// A needle occurrence found in a TABLE CELL. WholeDoc reaches these directly;
/// restricted top-level ranges surface them as out-of-scope matches. The
/// paragraph id also makes each occurrence reachable through SingleBlock.
/// A confident `applied=N` that omits this is the same dishonest-receipt class as
/// reporting a success the engine cannot substantiate.
#[derive(Clone, Debug)]
pub struct UnreachedCellMatch {
    /// The enclosing table block's id.
    pub table_id: NodeId,
    /// The addressable paragraph id inside the cell. Passing this as an
    /// explicit SingleBlock scope makes the same tracked splice reachable.
    pub paragraph_id: NodeId,
    /// Zero-based row index within the table.
    pub row: usize,
    /// Zero-based cell index within the row.
    pub col: usize,
    /// Capped surrounding context with the match delimited.
    pub excerpt: String,
}

/// Scan every table cell for `needle` (the same region/match logic the body scan
/// uses) so restricted scopes can disclose occurrences they did not reach.
/// Read-only — it mutates nothing; it exists so the receipt is honest and gives
/// the caller the paragraph id for an exact follow-up. Headers/footnotes/
/// textboxes are likewise out of body scope but are not walked here.
pub fn unreached_cell_matches(
    doc: &CanonDoc,
    needle: &str,
    mode: MatchMode,
) -> Vec<UnreachedCellMatch> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    for tb in &doc.blocks {
        let BlockNode::Table(table) = &tb.block else {
            continue;
        };
        for (r, row) in table.rows.iter().enumerate() {
            for (c, cell) in row.cells.iter().enumerate() {
                for blk in &cell.blocks {
                    let BlockNode::Paragraph(p) = blk else {
                        continue;
                    };
                    for region in partition_regions(p) {
                        let (spans, _) = region_matches(&region.text, needle, mode);
                        for span in spans {
                            out.push(UnreachedCellMatch {
                                table_id: table.id.clone(),
                                paragraph_id: p.id.clone(),
                                row: r,
                                col: c,
                                excerpt: build_excerpt(&region.text, span),
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

/// A match that could not be replaced because it crossed a wall, surfaced under
/// `BarrierPolicy::Skip` so a partial outcome is never silent.
#[derive(Clone, Debug)]
pub struct SkippedStraddle {
    pub block_id: NodeId,
    /// "anchor" or "tracked_segment" — which wall the match crossed.
    pub wall: &'static str,
}

/// The result of planning a `replace_text`: the splice steps, the replaced
/// sites, the normalization classes that fired, and any skipped straddles.
#[derive(Clone, Debug, Default)]
pub struct ReplaceTextPlan {
    pub steps: Vec<EditStep>,
    pub matches: Vec<MatchSite>,
    pub normalization_applied: Vec<NormalizationClass>,
    pub skipped_straddles: Vec<SkippedStraddle>,
}

/// Planner-level errors. `MatchCountMismatch` is NOT an `EditError` (it is a
/// count gate, not an apply failure) — it carries the per-site excerpts so the
/// caller disambiguates in exactly one follow-up.
#[derive(Clone, Debug)]
pub enum ReplaceTextError {
    Engine(EditError),
    MatchCountMismatch {
        expected: ExpectedMatches,
        actual: usize,
        sites: Vec<MatchSite>,
        /// Teaching diagnostics, populated ONLY on a ZERO-match mismatch (when
        /// `actual > 0` the per-site `sites` excerpts already serve). Each entry
        /// explains WHY the literal needle matched nothing and what to change — so
        /// the agent fixes the call in one follow-up instead of falling back to
        /// read_block/apply_edit ceremony (the benchmark's measured residual).
        ///
        /// `zero_match_diagnosis` is ONE general mechanism (a nearest-candidate
        /// reporter) with five CLASSIFICATIONS layered on it: (1)
        /// normalize_ws-would-match (the needle differs from body text only by
        /// whitespace/quote equivalence; names the classes and suggests
        /// `match_mode: "normalize_ws"`); (2) label-strip (the needle begins with a
        /// structural numbering label that lives in `literal_prefix`, not matchable
        /// body; suggests the label-stripped needle); (3) tracked-wall straddle
        /// (the needle spans an opaque anchor or a tracked-change boundary in a
        /// named block; not splice-replaceable); (4) already-applied (the
        /// REPLACEMENT text is already present where the needle would go — the
        /// duplicate-application / idempotency signature); (5) out-of-scope (matches
        /// exist OUTSIDE the given scope; widen it). When no classification fires
        /// but a near miss exists, the general nearest-candidate reporter names the
        /// closest body substring and the first concrete difference (block, offset,
        /// what differs) — catching the failure class the classifications don't
        /// model (a typo).
        ///
        /// Every entry fires ONLY when it would change the outcome — a genuinely
        /// absent needle yields an EMPTY vec (no speculative advice). Diagnosis
        /// INFORMS, never acts: nothing here changes matching behavior.
        diagnosis: Vec<String>,
    },
}

impl From<EditError> for ReplaceTextError {
    fn from(e: EditError) -> Self {
        ReplaceTextError::Engine(e)
    }
}

/// How many chars of context to show on each side of a match excerpt.
const EXCERPT_RADIUS: usize = 32;

/// A maximal run of text with one editable (`Normal` or `Inserted`) status in a
/// paragraph, with its flat-inline range.
///
/// `flat_start`/`flat_end` are half-open boundary indices into the SHARED
/// `flat_inlines(para)` — the exact index space the splice resolver
/// (`resolve_span`) consumes. They are NOT character offsets: the unit is one
/// whole inline. Because `partition_regions` derives them from
/// `flat_inlines_with_status` (the same flattening, paired with segment status),
/// a region's `[flat_start, flat_end)` selects EXACTLY the inlines whose text it
/// holds — true by construction, not by two iterations that happen to agree
/// (`region_flat_range_indexes_the_same_inlines_it_collected` pins this).
struct Region {
    text: String,
    /// `[flat_start, flat_end)` over `flat_inlines(para)`.
    flat_start: usize,
    flat_end: usize,
}

/// Coalesce the paragraph's flat inlines into maximal runs of text with the
/// same editable status ("regions"), separated by walls. Normal and Inserted
/// segments are editable; Deleted and InsertedThenDeleted remain walls. Opaque
/// anchors, hard breaks, and zero-width decorations are walls too. A region
/// never spans a status boundary, keeping attribution explicit.
///
/// The flat index space is the SHARED `flat_inlines_with_status(para)`, which is
/// `flat_inlines(para)` paired with each inline's segment status. So a region's
/// `flat_start`/`flat_end` are positions in the same flattening the resolver
/// indexes; we never re-walk `para.segments` with a private counter (the second
/// walk is exactly what used to drift from the resolver). One flattening, one
/// coordinate system.
fn partition_regions(para: &ParagraphNode) -> Vec<Region> {
    let flat = crate::edit::flat_inlines_with_status(para);

    let mut regions = Vec::new();
    let mut i = 0usize;
    while i < flat.len() {
        // A region starts at visible, replaceable text.
        if matches!(flat[i].0, InlineNode::Text(_))
            && matches!(
                flat[i].1,
                TrackingStatus::Normal | TrackingStatus::Inserted(_)
            )
        {
            let flat_start = i;
            let region_status = flat[i].1;
            let mut text = String::new();
            while i < flat.len() {
                if let InlineNode::Text(t) = flat[i].0
                    && flat[i].1 == region_status
                {
                    text.push_str(&t.text);
                    i += 1;
                } else {
                    break;
                }
            }
            regions.push(Region {
                text,
                flat_start,
                flat_end: i,
            });
        } else {
            // Wall (anchor, hard break, deletion/stacked text, or zero-width inline) —
            // advance one flat slot without opening a region.
            i += 1;
        }
    }
    regions
}

/// Fold one char to its normalization-class representative, returning the class
/// that fired (or `None` if the char is unchanged).
fn fold_char(c: char) -> (char, Option<NormalizationClass>) {
    match c {
        // Whitespace family → ASCII space.
        '\u{00A0}' | '\u{202F}' | '\u{2007}' | '\u{0009}' | '\u{2002}'..='\u{200A}' => {
            (' ', Some(NormalizationClass::Whitespace))
        }
        // Apostrophe / closing single quote → '.
        '\u{2019}' | '\u{02BC}' => ('\'', Some(NormalizationClass::Apostrophe)),
        // Opening single quote → '.
        '\u{2018}' => ('\'', Some(NormalizationClass::SingleQuote)),
        // Curly double quotes → ".
        '\u{201C}' | '\u{201D}' => ('"', Some(NormalizationClass::DoubleQuote)),
        _ => (c, None),
    }
}

/// Normalize a string for MATCHING ONLY: returns the folded chars used to
/// locate matches. The folded form NEVER reaches the document — `rewrite_region`
/// copies untouched chars from the ORIGINAL `region_text` and writes the literal
/// `new` verbatim, so untouched bytes stay byte-identical and the replacement
/// keeps its exact casing/codepoints. (Class accounting is done per-match in
/// `region_matches`, against the original chars.)
fn normalize_chars(chars: &[char], mode: MatchMode) -> Vec<char> {
    match mode {
        MatchMode::Exact => chars.to_vec(),
        MatchMode::NormalizeWs => chars.iter().map(|&c| fold_char(c).0).collect(),
    }
}

/// Does the (already folded) needle match the (already folded) haystack chars at
/// `i`? Literal, case-sensitive, no whole-word constraint (replace_text matches
/// a literal phrase).
fn folded_match_at(hay: &[char], i: usize, needle: &[char]) -> bool {
    i + needle.len() <= hay.len() && hay[i..i + needle.len()] == *needle
}

/// All non-overlapping match start offsets of `needle` in `region_text` under
/// `mode`, as ORIGINAL char indices. Also reports the union of normalization
/// classes that fired across the matched spans.
fn region_matches(
    region_text: &str,
    needle: &str,
    mode: MatchMode,
) -> (Vec<(usize, usize)>, Vec<NormalizationClass>) {
    let hay: Vec<char> = region_text.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let folded_hay = normalize_chars(&hay, mode);
    let folded_needle = normalize_chars(&needle_chars, mode);

    let mut spans = Vec::new();
    let mut classes: Vec<NormalizationClass> = Vec::new();
    let mut i = 0usize;
    while i + folded_needle.len() <= folded_hay.len() {
        if folded_match_at(&folded_hay, i, &folded_needle) {
            let end = i + folded_needle.len();
            // Record which classes fired inside this match (compare originals).
            if mode == MatchMode::NormalizeWs {
                for &c in &hay[i..end] {
                    if let (_, Some(class)) = fold_char(c)
                        && !classes.contains(&class)
                    {
                        classes.push(class);
                    }
                }
            }
            spans.push((i, end));
            i = end;
        } else {
            i += 1;
        }
    }
    (spans, classes)
}

/// Build the replacement text for a region given its matches (original char
/// indices) and the literal `new`. Untouched chars are copied verbatim.
fn rewrite_region(region_text: &str, spans: &[(usize, usize)], new: &str) -> String {
    let chars: Vec<char> = region_text.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut span_iter = spans.iter().peekable();
    while i < chars.len() {
        if let Some(&&(start, end)) = span_iter.peek()
            && i == start
        {
            out.push_str(new);
            i = end;
            span_iter.next();
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// A capped excerpt around the first match in a region, with the match delimited
/// by «»: `…context «old» context…`.
fn build_excerpt(region_text: &str, (start, end): (usize, usize)) -> String {
    let chars: Vec<char> = region_text.chars().collect();
    let from = start.saturating_sub(EXCERPT_RADIUS);
    let to = (end + EXCERPT_RADIUS).min(chars.len());
    let mut s = String::new();
    if from > 0 {
        s.push('…');
    }
    s.extend(&chars[from..start]);
    s.push('«');
    s.extend(&chars[start..end]);
    s.push('»');
    s.extend(&chars[end..to]);
    if to < chars.len() {
        s.push('…');
    }
    s
}

/// Resolve the scope to the ordered list of (block_id, paragraph) pairs to
/// search. Fails loud on an unknown/mis-ordered block id.
fn scoped_paragraphs<'a>(
    doc: &'a CanonDoc,
    scope: &ReplaceTextScope,
) -> Result<Vec<&'a ParagraphNode>, EditError> {
    let body_paras: Vec<&ParagraphNode> = doc
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p.as_ref()),
            _ => None,
        })
        .collect();
    match scope {
        ReplaceTextScope::WholeDoc => Ok(body_paras),
        ReplaceTextScope::BodyAndTables => {
            fn collect<'a>(block: &'a BlockNode, out: &mut Vec<&'a ParagraphNode>) {
                match block {
                    BlockNode::Paragraph(p) => out.push(p.as_ref()),
                    BlockNode::Table(table) => {
                        for row in &table.rows {
                            for cell in &row.cells {
                                for block in &cell.blocks {
                                    collect(block, out);
                                }
                            }
                        }
                    }
                    BlockNode::OpaqueBlock(_) => {}
                }
            }

            let mut paragraphs = Vec::new();
            for tracked in &doc.blocks {
                collect(&tracked.block, &mut paragraphs);
            }
            Ok(paragraphs)
        }
        ReplaceTextScope::SingleBlock(id) => {
            let path = find_paragraph_path(doc, id).ok_or_else(|| EditError::BlockNotFound {
                block_id: id.clone(),
                step_index: 0,
            })?;
            let p = match block_at(doc, &path) {
                BlockNode::Paragraph(p) => p.as_ref(),
                BlockNode::Table(_) => {
                    return Err(EditError::NotAParagraph {
                        block_id: id.clone(),
                        actual_kind: "table",
                        step_index: 0,
                    });
                }
                BlockNode::OpaqueBlock(_) => {
                    return Err(EditError::NotAParagraph {
                        block_id: id.clone(),
                        actual_kind: "opaque_block",
                        step_index: 0,
                    });
                }
            };
            if &p.id != id {
                return Err(EditError::BlockNotFound {
                    block_id: id.clone(),
                    step_index: 0,
                });
            }
            Ok(vec![p])
        }
        ReplaceTextScope::Range { from, to } => {
            let start = body_paras
                .iter()
                .position(|p| &p.id == from)
                .ok_or_else(|| EditError::BlockNotFound {
                    block_id: from.clone(),
                    step_index: 0,
                })?;
            let end = body_paras.iter().position(|p| &p.id == to).ok_or_else(|| {
                EditError::BlockNotFound {
                    block_id: to.clone(),
                    step_index: 0,
                }
            })?;
            if start > end {
                return Err(EditError::BlockRangeInvalid {
                    start_block_id: from.clone(),
                    end_block_id: to.clone(),
                    reason: "replace_text scope `from` block comes after `to` in document order",
                    step_index: 0,
                });
            }
            Ok(body_paras[start..=end].to_vec())
        }
    }
}

/// Plan a tracked-native find/replace. Two passes: (1) census every replaceable
/// match against the scope and gate on `expected`; (2) only if the gate passes,
/// build one `ReplaceSpanText` splice per affected paragraph. Returns the plan or
/// a `ReplaceTextError` (engine refusal, or a count-mismatch carrying the site
/// excerpts).
pub fn plan_replace_text(
    doc: &CanonDoc,
    opts: &ReplaceTextOptions,
) -> Result<ReplaceTextPlan, ReplaceTextError> {
    if opts.old.is_empty() {
        return Err(ReplaceTextError::Engine(EditError::NoOpEdit {
            block_id: NodeId::from(""),
            step_index: 0,
            reason: "replace_text `old` is empty",
        }));
    }

    let paras = scoped_paragraphs(doc, &opts.scope)?;

    // ── Pass 1: census + straddle detection ────────────────────────────────
    let mut sites: Vec<MatchSite> = Vec::new();
    let mut skipped: Vec<SkippedStraddle> = Vec::new();
    let mut classes: Vec<NormalizationClass> = Vec::new();

    for para in &paras {
        // A tracked BLOCK status can't be carried by a splice — refuse up front.
        let top_level_status =
            find_paragraph_path(doc, &para.id).map(|path| &doc.blocks[path.top_block].status);
        if let Some(status) = top_level_status
            && *status != TrackingStatus::Normal
        {
            // Only refuse if this paragraph actually contains a match; otherwise
            // a tracked block elsewhere shouldn't block an unrelated replace.
            if paragraph_has_match(para, opts) {
                return Err(ReplaceTextError::Engine(EditError::BlockHasTrackedStatus {
                    block_id: para.id.clone(),
                    status: tracked_status_label(status),
                    step_index: 0,
                }));
            }
            continue;
        }

        let regions = partition_regions(para);
        let mut had_region_match = false;
        for region in &regions {
            let (spans, cls) = region_matches(&region.text, &opts.old, opts.match_mode);
            for c in cls {
                if !classes.contains(&c) {
                    classes.push(c);
                }
            }
            for &span in &spans {
                had_region_match = true;
                sites.push(MatchSite {
                    block_id: para.id.clone(),
                    excerpt: build_excerpt(&region.text, span),
                });
            }
        }

        // Straddle detection: a needle present across the paragraph's editable text
        // but absent from every region crossed a wall.
        if !had_region_match {
            let joined: String = regions.iter().map(|r| r.text.as_str()).collect();
            let (whole_spans, _) = region_matches(&joined, &opts.old, opts.match_mode);
            if !whole_spans.is_empty() {
                match opts.on_barrier_match {
                    BarrierPolicy::Skip => skipped.push(SkippedStraddle {
                        block_id: para.id.clone(),
                        wall: "anchor_or_tracked_segment",
                    }),
                    BarrierPolicy::Fail => {
                        return Err(ReplaceTextError::Engine(
                            EditError::FindReplaceBarrierStraddle {
                                block_id: para.id.clone(),
                                needle: opts.old.clone(),
                                step_index: 0,
                            },
                        ));
                    }
                }
            }
        }
    }

    // ── Gate on expected_matches ───────────────────────────────────────────
    let actual = sites.len();
    match opts.expected {
        ExpectedMatches::All => {}
        ExpectedMatches::Count(n) if n == actual => {}
        ExpectedMatches::Count(_) => {
            // On a ZERO-match mismatch, diagnose WHY (the benchmark's measured
            // residual: bare zero-match errors forced fallback ceremony). The
            // probes fire only when they'd change the outcome; empty otherwise.
            let diagnosis = if actual == 0 {
                zero_match_diagnosis(doc, &paras, opts)
            } else {
                Vec::new()
            };
            return Err(ReplaceTextError::MatchCountMismatch {
                expected: opts.expected,
                actual,
                sites,
                diagnosis,
            });
        }
    }

    // ── Pass 2: build one splice per affected paragraph ────────────────────
    let mut steps: Vec<EditStep> = Vec::new();
    for para in &paras {
        if let Some(step) = plan_paragraph_splice(para, opts)? {
            steps.push(step);
        }
    }

    Ok(ReplaceTextPlan {
        steps,
        matches: sites,
        normalization_applied: classes,
        skipped_straddles: skipped,
    })
}

/// Build at most one `ReplaceSpanText` for a paragraph: target the minimal
/// flat-inline range covering all matched regions and pass its whole rewritten
/// text. Refuses (per barrier policy) when matched regions are separated by a
/// deletion or terminal stacked segment.
fn plan_paragraph_splice(
    para: &ParagraphNode,
    opts: &ReplaceTextOptions,
) -> Result<Option<EditStep>, EditError> {
    let regions = partition_regions(para);
    // Find regions that actually change.
    let mut matched: Vec<(&Region, Vec<(usize, usize)>)> = Vec::new();
    for region in &regions {
        let (spans, _) = region_matches(&region.text, &opts.old, opts.match_mode);
        if !spans.is_empty() {
            matched.push((region, spans));
        }
    }
    if matched.is_empty() {
        return Ok(None);
    }

    let cover_start = matched.first().unwrap().0.flat_start;
    let cover_end = matched.last().unwrap().0.flat_end;

    // The covering range may contain Normal text and pending insertions, whose
    // attribution the splice materializer preserves/stacks. A deletion or
    // terminal stacked segment remains a wall. Anchors inside the range are
    // carried by reference.
    if range_contains_unreplaceable_inline(para, cover_start, cover_end) {
        match opts.on_barrier_match {
            BarrierPolicy::Skip => return Ok(None),
            BarrierPolicy::Fail => {
                return Err(EditError::SpanCrossesTrackedSegment {
                    block_id: para.id.clone(),
                    step_index: 0,
                });
            }
        }
    }

    // Build the covering range's replacement content: walk the flat inlines in
    // [cover_start, cover_end), emitting rewritten editable Text and a
    // PreservedInlineRef for each anchor (so opaques survive). A region's matches
    // are applied; non-matched editable text is copied verbatim.
    let content = build_covering_content(para, &matched, cover_start, cover_end, &opts.new);
    let expect = flat_range_visible_text(para, cover_start, cover_end);
    let guard = block_semantic_hash_for_block(&BlockNode::from(para.clone()));

    let span = flat_range_to_selector(para, cover_start, cover_end);

    Ok(Some(EditStep::ReplaceSpanText {
        block_id: para.id.clone(),
        guard,
        expect: Some(expect),
        span,
        content,
        rationale: None,
    }))
}

/// Does `[start, end)` contain text from a segment the tracked splice cannot
/// edit? Normal and Inserted are admitted; Deleted and the terminal stacked
/// state are walls. Indexes the shared flattening used by the resolver.
fn range_contains_unreplaceable_inline(para: &ParagraphNode, start: usize, end: usize) -> bool {
    crate::edit::flat_inlines_with_status(para)[start..end]
        .iter()
        .any(|(_, status)| {
            matches!(
                status,
                TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_)
            )
        })
}

/// The visible text of `[start, end)` flat inlines (Text only; anchors contribute
/// nothing) — the `expect` precondition for the splice. Indexes the SHARED
/// `flat_inlines` so the range maps to exactly the inlines the resolver sees.
fn flat_range_visible_text(para: &ParagraphNode, start: usize, end: usize) -> String {
    let mut out = String::new();
    for inline in &crate::edit::flat_inlines(para)[start..end] {
        if let InlineNode::Text(t) = inline {
            out.push_str(&t.text);
        }
    }
    out
}

/// Build the `ParagraphContent` for the covering range `[start, end)`: walk the
/// flat inlines in order; when entering a matched region (at its `flat_start`)
/// emit its rewritten text and skip the rest of the region's inlines; otherwise
/// emit each editable text inline verbatim (a non-matched gap between matched
/// regions) and each anchor as a `PreservedInlineRef` (so opaques survive).
/// Pending insertions may appear; deletion and terminal stacked text were
/// already refused.
fn build_covering_content(
    para: &ParagraphNode,
    matched: &[(&Region, Vec<(usize, usize)>)],
    start: usize,
    end: usize,
    new: &str,
) -> ParagraphContent {
    use std::collections::HashMap;
    // region flat_start -> (flat_end, rewritten region text)
    let region_info: HashMap<usize, (usize, String)> = matched
        .iter()
        .map(|(r, spans)| {
            (
                r.flat_start,
                (r.flat_end, rewrite_region(&r.text, spans, new)),
            )
        })
        .collect();

    let mut fragments: Vec<ContentFragment> = Vec::new();
    let mut skip_until: Option<usize> = None;

    // Iterate the SHARED flattening; `flat` is the position in `flat_inlines`,
    // the same index space `region_info` (region.flat_start/end) and the
    // resolver use.
    for (flat, inline) in crate::edit::flat_inlines(para).iter().enumerate() {
        // Skip the trailing inlines of a matched region whose rewrite we
        // already emitted at its flat_start.
        if let Some(until) = skip_until {
            if flat < until {
                continue;
            }
            skip_until = None;
        }
        if flat < start || flat >= end {
            continue;
        }
        if let Some((region_end, rewritten)) = region_info.get(&flat) {
            fragments.push(ContentFragment::Text(rewritten.clone()));
            skip_until = Some(*region_end);
            continue;
        }
        match inline {
            InlineNode::OpaqueInline(o) => {
                fragments.push(ContentFragment::PreservedInlineRef(o.id.clone()));
            }
            InlineNode::HardBreak(hb) => {
                fragments.push(ContentFragment::PreservedInlineRef(hb.id.clone()));
            }
            // An editable text inline in a non-matched gap between matched
            // regions — copy verbatim.
            InlineNode::Text(t) => fragments.push(ContentFragment::Text(t.text.clone())),
            _ => {}
        }
    }

    ParagraphContent { fragments }
}

/// Pick the splice selector for the flat range `[start, end)`: `Whole` when the
/// range is the entire paragraph, else `Between` with `FlatIndex` (or `Start`/
/// `End`) endpoints.
fn flat_range_to_selector(para: &ParagraphNode, start: usize, end: usize) -> ResolvedSpanSelector {
    let total = flat_inline_count(para);
    if start == 0 && end == total {
        return ResolvedSpanSelector::Whole;
    }
    let start_ep = if start == 0 {
        ResolvedSpanEndpoint::Start
    } else {
        ResolvedSpanEndpoint::FlatIndex(start)
    };
    let end_ep = if end == total {
        ResolvedSpanEndpoint::End
    } else {
        ResolvedSpanEndpoint::FlatIndex(end)
    };
    ResolvedSpanSelector::Between {
        start: start_ep,
        end: end_ep,
    }
}

/// The total flat-inline count — the resolver's `total` and the upper boundary
/// of any `FlatIndex`. Same flattening as everything else here.
fn flat_inline_count(para: &ParagraphNode) -> usize {
    crate::edit::flat_inlines(para).len()
}

/// Cheap "does this paragraph contain a replaceable match" check (region-local).
fn paragraph_has_match(para: &ParagraphNode, opts: &ReplaceTextOptions) -> bool {
    partition_regions(para).iter().any(|r| {
        !region_matches(&r.text, &opts.old, opts.match_mode)
            .0
            .is_empty()
    })
}

/// Diagnose a ZERO-match: explain why the literal needle matched nothing and what
/// to change. ONE general mechanism (the nearest-candidate reporter) with five
/// CLASSIFICATIONS layered on it. The classifications, in order: (1)
/// normalize_ws-would-match — the needle differs from body text only by
/// whitespace/quote equivalence, names the classes that would fire; (2)
/// label-strip — the needle begins with a structural numbering label that lives in
/// `literal_prefix`, not matchable body, suggests the stripped needle; (3)
/// tracked-wall straddle — the needle spans an opaque anchor or a tracked-change
/// boundary, names the block and the wall; (4) already-applied — the REPLACEMENT
/// text is already present where the needle should be (the duplicate-application
/// signature, a quiet idempotency story); (5) out-of-scope — matches exist OUTSIDE
/// the given scope (one bounded whole-body scan), names how many and the scope
/// they fell outside.
///
/// When no classification fired but a near miss exists, the general
/// nearest-candidate reporter names the closest body substring and the FIRST
/// concrete difference (block id, offset, what differs) — this catches the
/// failure class the classifications don't model (e.g. a typo'd needle).
///
/// Every entry fires ONLY when it would change the outcome — a genuinely-absent
/// needle (no near candidate, no out-of-scope match, replacement not present)
/// yields an EMPTY vec. The diagnosis INFORMS, never acts: nothing here changes
/// matching behavior (auto-applying normalize_ws on a zero-match would be a silent
/// fallback wearing a helpful hat). Bounded: the classifications and the
/// nearest-candidate scan run over the scoped `paras`; the out-of-scope probe gets
/// exactly one bounded whole-body scan. Zero-match path only.
fn zero_match_diagnosis(
    doc: &CanonDoc,
    paras: &[&ParagraphNode],
    opts: &ReplaceTextOptions,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = classify_normalize_ws(paras, opts) {
        out.push(m);
    }
    if let Some(m) = classify_label_strip(paras, opts) {
        out.push(m);
    }
    if let Some(m) = classify_tracked_wall_straddle(paras, opts) {
        out.push(m);
    }
    if let Some(m) = classify_already_applied(paras, opts) {
        out.push(m);
    }
    if let Some(m) = classify_out_of_scope(doc, opts) {
        out.push(m);
    }

    // The general base only speaks up when no classification did AND there is a
    // genuinely close candidate — otherwise it would either repeat a class we
    // already named precisely, or invent advice for an absent needle.
    if out.is_empty()
        && let Some(m) = nearest_candidate_report(paras, opts)
    {
        out.push(m);
    }
    out
}

/// Classification 1: the needle differs from body text only by whitespace/quote
/// equivalence. Fires ONLY when match_mode is currently Exact (under normalize_ws
/// it already folds, so the advice wouldn't help) AND re-matching under
/// normalize_ws yields ≥1 match. Names the equivalence classes that fired, so the
/// advice mirrors the receipt's `normalization_applied` reporting.
fn classify_normalize_ws(paras: &[&ParagraphNode], opts: &ReplaceTextOptions) -> Option<String> {
    if opts.match_mode != MatchMode::Exact {
        return None;
    }
    let mut classes: Vec<NormalizationClass> = Vec::new();
    let mut any = false;
    for para in paras {
        for region in partition_regions(para) {
            let (spans, fired) = region_matches(&region.text, &opts.old, MatchMode::NormalizeWs);
            if !spans.is_empty() {
                any = true;
                for c in fired {
                    if !classes.contains(&c) {
                        classes.push(c);
                    }
                }
            }
        }
    }
    if !any || classes.is_empty() {
        return None;
    }
    let names: Vec<&str> = classes.iter().map(|c| c.as_str()).collect();
    Some(format!(
        "the needle matches body text under whitespace/quote equivalence ({}) but not \
         byte-for-byte — retry with match_mode: \"normalize_ws\"",
        names.join(", ")
    ))
}

/// Classification 2: the needle begins with a structural numbering label
/// (`match_prefix_pattern` — the recognizer import uses to hoist labels). The
/// label lives in `literal_prefix`, never in matchable body text. Fires ONLY when
/// the label-stripped needle WOULD match in scope. Intent-preserving per the
/// prefix-duplication contract: tell the agent to drop the label from the needle.
fn classify_label_strip(paras: &[&ParagraphNode], opts: &ReplaceTextOptions) -> Option<String> {
    let trimmed = opts.old.trim_start_matches([' ', '\t']);
    let (label, consumed) = crate::import::match_prefix_pattern(trimmed)?;
    let stripped = trimmed[consumed..].to_string();
    if stripped.is_empty() {
        return None;
    }
    let probe = ReplaceTextOptions {
        old: stripped.clone(),
        ..opts.clone()
    };
    if !paras.iter().any(|p| paragraph_has_match(p, &probe)) {
        return None;
    }
    Some(format!(
        "the needle begins with the numbering label '{label}', which is structural \
         (it lives in the paragraph's numbering, not matchable body text) — retry \
         with '{stripped}' (the label-stripped text)"
    ))
}

/// Classification 3: the needle spans a wall — an opaque anchor or a tracked-change
/// boundary — so it is present in a paragraph's joined Normal text but in no
/// single region, and a splice can't replace across it. Reuses the SAME straddle
/// test the census uses (needle in the joined regions but in no region). Fires
/// only when such a straddle exists; names the first block and the wall kind.
fn classify_tracked_wall_straddle(
    paras: &[&ParagraphNode],
    opts: &ReplaceTextOptions,
) -> Option<String> {
    for para in paras {
        let regions = partition_regions(para);
        let region_local = regions.iter().any(|r| {
            !region_matches(&r.text, &opts.old, opts.match_mode)
                .0
                .is_empty()
        });
        if region_local {
            continue; // matched in a region — not a straddle
        }
        let joined: String = regions.iter().map(|r| r.text.as_str()).collect();
        if !region_matches(&joined, &opts.old, opts.match_mode)
            .0
            .is_empty()
        {
            return Some(format!(
                "the needle spans a wall (an opaque anchor or a tracked-change boundary) in \
                 block '{}' — replace_text only edits within one unbroken run of body text, \
                 so split the edit around the wall or target the surrounding text",
                para.id.0
            ));
        }
    }
    None
}

/// Classification 4: the REPLACEMENT text (`new`) is already present where the
/// needle should be — the duplicate-application signature after an ambiguous
/// failure. Fires ONLY when `new` is non-empty, differs from `old`, and matches in
/// the scoped body under the active match mode. This quietly buys most of an
/// idempotency story: "you (or a prior call) likely already applied this."
fn classify_already_applied(paras: &[&ParagraphNode], opts: &ReplaceTextOptions) -> Option<String> {
    if opts.new.is_empty() || opts.new == opts.old {
        return None;
    }
    let probe = ReplaceTextOptions {
        old: opts.new.clone(),
        ..opts.clone()
    };
    let block = paras.iter().find(|p| paragraph_has_match(p, &probe))?;
    Some(format!(
        "the replacement text '{}' is already present in block '{}' where the needle would \
         go — the change appears already applied (a prior call likely succeeded); nothing to do",
        opts.new, block.id.0
    ))
}

/// Classification 5: matches for the needle exist OUTSIDE the given scope. Fires
/// ONLY when the scope is restricted (Range/SingleBlock — WholeDoc has no
/// "outside") AND a bounded whole-body scan finds ≥1 match in a body paragraph not
/// in the scoped set. Names the count and the scope they fell outside, so the
/// agent widens the scope rather than concluding the phrase is absent.
fn classify_out_of_scope(doc: &CanonDoc, opts: &ReplaceTextOptions) -> Option<String> {
    let scope_label = match &opts.scope {
        // Both broad scopes cover every region they promise. BodyAndTables is
        // the MCP scope; WholeDoc is the frozen CLI's top-level body scope.
        ReplaceTextScope::WholeDoc | ReplaceTextScope::BodyAndTables => return None,
        ReplaceTextScope::SingleBlock(id) => format!("block '{}'", id.0),
        ReplaceTextScope::Range { from, to } => {
            format!("block range '{}'..'{}'", from.0, to.0)
        }
    };

    // The scoped paragraph ids (the in-scope set). If the scope id is unknown,
    // `scoped_paragraphs` already failed earlier — here it resolves.
    let scoped: std::collections::HashSet<&NodeId> = match scoped_paragraphs(doc, &opts.scope) {
        Ok(ps) => ps.iter().map(|p| &p.id).collect(),
        Err(_) => return None,
    };

    // One bounded whole-body scan over the paragraphs NOT in scope.
    let mut outside_hits = 0usize;
    let mut first_outside: Option<&NodeId> = None;
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && !scoped.contains(&p.id)
            && paragraph_has_match(p, opts)
        {
            outside_hits += 1;
            if first_outside.is_none() {
                first_outside = Some(&p.id);
            }
        }
    }
    if outside_hits == 0 {
        return None;
    }
    let where_: &str = first_outside.map(|id| &*id.0).unwrap_or("the body");
    Some(format!(
        "the needle matches nothing inside {scope_label} but {outside_hits} time(s) \
         OUTSIDE it (first at block '{where_}') — widen the scope to reach those matches"
    ))
}

/// The GENERAL nearest-candidate reporter: scan the scoped body for the substring
/// closest to the needle and, if one is genuinely close, name it with the FIRST
/// concrete difference (block id, char offset, what differs). This is the base
/// mechanism the classifications layer on — it catches the failure class they
/// don't model (a typo, a transposed word) without inventing advice for a truly
/// absent needle.
///
/// "Close" is bounded: we only report a candidate whose character-mismatch count
/// is at most a small fraction of the needle length (`<= max(1, len/4)`), so a
/// needle with no near neighbour yields `None`. The scan slides a needle-length
/// window over each region's text (under the active match mode's folding for the
/// comparison) and keeps the global minimum.
fn nearest_candidate_report(paras: &[&ParagraphNode], opts: &ReplaceTextOptions) -> Option<String> {
    let needle: Vec<char> = opts.old.chars().collect();
    if needle.is_empty() {
        return None;
    }
    let folded_needle = normalize_chars(&needle, opts.match_mode);

    // The closeness budget: at most a quarter of the needle (min 1) may differ.
    // A larger gap means "not really the same phrase" — stay silent.
    let budget = (needle.len() / 4).max(1);

    let mut best: Option<(usize, &NodeId, usize, char, char)> = None; // (mismatches, block, offset, want, got)
    for para in paras {
        for region in partition_regions(para) {
            let hay: Vec<char> = region.text.chars().collect();
            if hay.len() < needle.len() {
                continue;
            }
            let folded_hay = normalize_chars(&hay, opts.match_mode);
            for start in 0..=(folded_hay.len() - folded_needle.len()) {
                let mut mismatches = 0usize;
                let mut first_diff: Option<(usize, char, char)> = None;
                for k in 0..folded_needle.len() {
                    if folded_hay[start + k] != folded_needle[k] {
                        mismatches += 1;
                        if first_diff.is_none() {
                            // Report the ORIGINAL chars (what the agent sees),
                            // not the folded ones.
                            first_diff = Some((start + k, needle[k], hay[start + k]));
                        }
                    }
                }
                if mismatches == 0 {
                    // A zero-mismatch window would have matched the census — it
                    // cannot happen on the zero-match path, but guard anyway.
                    continue;
                }
                if mismatches <= budget {
                    let better = best.is_none_or(|(bm, ..)| mismatches < bm);
                    if better && let Some((off, want, got)) = first_diff {
                        best = Some((mismatches, &para.id, off, want, got));
                    }
                }
            }
        }
    }

    let (mismatches, block, offset, want, got) = best?;
    Some(format!(
        "no exact match, but block '{}' has a near miss ({mismatches} char(s) differ): at \
         offset {offset} the needle wants {} where the document has {} — check for a typo or \
         a different word",
        block.0,
        describe_char(want),
        describe_char(got),
    ))
}

/// A short, readable rendering of a single char for a diff message: printable
/// chars as `'x'`, otherwise the Unicode scalar `U+00A0`.
fn describe_char(c: char) -> String {
    if c.is_control() || c.is_whitespace() && c != ' ' {
        format!("U+{:04X}", c as u32)
    } else {
        format!("'{c}'")
    }
}

fn tracked_status_label(status: &TrackingStatus) -> &'static str {
    match status {
        TrackingStatus::Normal => "normal",
        TrackingStatus::Inserted(_) => "inserted",
        TrackingStatus::Deleted(_) => "deleted",
        TrackingStatus::InsertedThenDeleted(_) => "inserted_then_deleted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(needle: &str, replacement: &str) -> FindReplaceOptions {
        FindReplaceOptions {
            needle: needle.to_string(),
            replacement: replacement.to_string(),
            scope: FindReplaceScope::BodyOnly,
            case_sensitive: true,
            whole_word: false,
            on_barrier_match: BarrierPolicy::Skip,
        }
    }

    #[test]
    fn replace_in_section_replaces_all_non_overlapping() {
        let o = opts("ab", "X");
        assert_eq!(replace_in_section("abcabcab", &o), "XcXcX");
    }

    #[test]
    fn replace_in_section_no_match_is_identity() {
        let o = opts("zzz", "X");
        assert_eq!(replace_in_section("hello world", &o), "hello world");
    }

    #[test]
    fn case_insensitive_matches_but_writes_literal_replacement() {
        let mut o = opts("color", "colour");
        o.case_sensitive = false;
        assert_eq!(
            replace_in_section("Color COLOR color", &o),
            "colour colour colour"
        );
    }

    #[test]
    fn case_sensitive_does_not_match_other_casing() {
        let o = opts("color", "colour");
        assert_eq!(replace_in_section("Color color", &o), "Color colour");
    }

    #[test]
    fn whole_word_respects_unicode_alphanumeric_boundary() {
        let mut o = opts("cat", "dog");
        o.whole_word = true;
        // "category" must NOT match; "(cat)" and edge "cat" must.
        assert_eq!(
            replace_in_section("category cat (cat) cats", &o),
            "category dog (dog) cats"
        );
    }

    #[test]
    fn whole_word_matches_at_string_edges() {
        let mut o = opts("cat", "dog");
        o.whole_word = true;
        assert_eq!(replace_in_section("cat", &o), "dog");
        assert_eq!(replace_in_section("a cat", &o), "a dog");
    }

    #[test]
    fn find_match_locates_first_occurrence() {
        let o = opts("b", "X");
        assert_eq!(find_match("aabba", &o), Some(2));
        assert_eq!(find_match("aaa", &o), None);
    }

    // ─── cond-2: the matcher and the resolver share ONE flattening ───────────
    //
    // `partition_regions` (the matcher's coordinate space) and `flat_inlines`
    // (the resolver's coordinate space) must be the SAME index space — not two
    // walks that happen to agree. These tests pin that by construction: a
    // region's `[flat_start, flat_end)`, sliced into `flat_inlines(para)`, is
    // EXACTLY the inlines whose text the region holds.

    // `InlineNode`, `ParagraphNode`, `NodeId`, `TrackingStatus` are already in
    // scope via `super::*`; bring in only the fixture-only names.
    use crate::domain::{RevisionInfo, StyleProps, TextNode, TrackedSegment};
    use crate::edit::{flat_inlines, flat_inlines_with_status};

    fn text_inline(id: &str, text: &str) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(id),
            text_role: None,
            text: text.to_string(),
            marks: vec![],
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            formatting_change: None,
        })
    }

    fn inserted(author: &str) -> TrackingStatus {
        TrackingStatus::Inserted(RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some(author.to_string()),
            date: None,
            apply_op_id: None,
        })
    }

    fn deleted(author: &str) -> TrackingStatus {
        TrackingStatus::Deleted(RevisionInfo {
            revision_id: 2,
            identity: 0,
            author: Some(author.to_string()),
            date: None,
            apply_op_id: None,
        })
    }

    /// Build a paragraph from `(status, [(id, text)])` segments — minimal, with
    /// every non-segment field defaulted via `new_story_body` then overwritten.
    fn para_with_segments(
        id: &str,
        segs: Vec<(TrackingStatus, Vec<(&str, &str)>)>,
    ) -> ParagraphNode {
        let mut para = ParagraphNode::new_story_body(id, "", None);
        para.segments = segs
            .into_iter()
            .map(|(status, inlines)| TrackedSegment {
                status,
                inlines: inlines
                    .into_iter()
                    .map(|(iid, t)| text_inline(iid, t))
                    .collect(),
            })
            .collect();
        para
    }

    /// The text of `flat_inlines(para)[start..end]` (Text only) — what the
    /// resolver-side flat range actually contains.
    fn flat_range_text(para: &ParagraphNode, start: usize, end: usize) -> String {
        flat_inlines(para)[start..end]
            .iter()
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn editable_region_flat_ranges_share_resolver_coordinates_and_deletions_stay_walls() {
        // Normal | INSERTED | DELETED | Normal. The insertion is its own
        // editable region; the deletion remains an unreplaceable wall.
        let para = para_with_segments(
            "p_rt",
            vec![
                (TrackingStatus::Normal, vec![("r0", "the cat ")]),
                (inserted("Stemma"), vec![("r1", "big ")]),
                (deleted("Other"), vec![("r2", "old ")]),
                (TrackingStatus::Normal, vec![("r3", "cat sat")]),
            ],
        );

        let regions = partition_regions(&para);
        assert_eq!(
            regions.len(),
            3,
            "Normal, Inserted, and trailing Normal are independently editable"
        );
        assert_eq!(regions[0].text, "the cat ");
        assert_eq!(regions[1].text, "big ");
        assert_eq!(regions[2].text, "cat sat");

        // THE round-trip pin: each region's flat range, sliced into the SHARED
        // flat_inlines, is exactly the inlines whose text the region holds.
        for region in &regions {
            assert_eq!(
                flat_range_text(&para, region.flat_start, region.flat_end),
                region.text,
                "region [{}, {}) must index back to its own text in flat_inlines",
                region.flat_start,
                region.flat_end,
            );
        }

        // And the two flattenings agree on length and order by construction.
        assert_eq!(
            flat_inlines(&para).len(),
            flat_inlines_with_status(&para).len(),
        );

        // The deletion between the inserted and trailing Normal regions makes
        // a covering splice unreplaceable.
        let cover_start = regions[0].flat_start;
        let cover_end = regions[2].flat_end;
        assert!(
            range_contains_unreplaceable_inline(&para, cover_start, cover_end),
            "the covering range [{cover_start}, {cover_end}) spans the deleted wall",
        );
        for region in &regions {
            assert!(!range_contains_unreplaceable_inline(
                &para,
                region.flat_start,
                region.flat_end
            ));
        }
    }

    #[test]
    fn paragraph_splice_planner_targets_text_inside_a_pending_insertion() {
        let para = para_with_segments(
            "p_inserted",
            vec![(
                inserted("Prior Counsel"),
                vec![("r_inserted", "rate of 8% above base rate")],
            )],
        );
        let options = ReplaceTextOptions {
            old: "8%".to_string(),
            new: "2%".to_string(),
            author: "Reviewing Counsel".to_string(),
            scope: ReplaceTextScope::SingleBlock(NodeId::from("p_inserted")),
            expected: ExpectedMatches::Count(1),
            match_mode: MatchMode::Exact,
            on_barrier_match: BarrierPolicy::Fail,
        };

        let regions = partition_regions(&para);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].text, "rate of 8% above base rate");
        let step = plan_paragraph_splice(&para, &options)
            .expect("pending insertion is a valid tracked splice target")
            .expect("the matching insertion produces a step");
        match step {
            EditStep::ReplaceSpanText { block_id, .. } => {
                assert_eq!(block_id, NodeId::from("p_inserted"));
            }
            other => panic!("expected ReplaceSpanText, got {other:?}"),
        }
    }

    #[test]
    fn region_indices_account_for_anchor_walls() {
        // Normal "see " | HARD-BREAK anchor | Normal " end" — an anchor is a wall
        // too, and it occupies a flat slot, so the second region's flat_start must
        // skip past it.
        use crate::domain::{BreakType, HardBreakNode};
        let mut para = ParagraphNode::new_story_body("p_anchor", "", None);
        para.segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![
                text_inline("r0", "see "),
                InlineNode::HardBreak(HardBreakNode {
                    id: NodeId::from("br0"),
                    break_type: BreakType::TextWrapping,
                }),
                text_inline("r1", " end"),
            ],
        }];

        let regions = partition_regions(&para);
        assert_eq!(
            regions.len(),
            2,
            "the anchor splits the Normal text into two regions"
        );
        assert_eq!(regions[0].text, "see ");
        assert_eq!(regions[1].text, " end");
        // The second region begins AFTER the anchor's flat slot (index 2), not at
        // index 1 — proving the region indices account for the anchor slot.
        assert_eq!(regions[1].flat_start, 2);
        for region in &regions {
            assert_eq!(
                flat_range_text(&para, region.flat_start, region.flat_end),
                region.text,
            );
        }
    }
}
