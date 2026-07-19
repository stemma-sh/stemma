//! `replace_text` — tracked-native find/replace that SPLICES through paragraphs
//! already carrying tracked changes.
//!
//! Domain rules pinned here:
//!   - a match in a CLEAN paragraph behaves like a whole-paragraph replace;
//!   - a match in the Normal region of an ALREADY-TRACKED paragraph splices: the
//!     pre-existing tracked markup is carried through byte/status/author-identical;
//!   - `expected_matches` (default 1) is a hard gate: actual != expected fails
//!     with the per-site excerpts; "all" replaces everywhere;
//!   - `normalize_ws` matches across whitespace/quote equivalence classes and
//!     REPORTS the classes that fired;
//!   - a match straddling a wall (opaque anchor or tracked-segment boundary) is
//!     skipped|failed, never half-applied.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::edit::{
    BarrierPolicy, EditTransaction, ExpectedMatches, MatchMode, MaterializationMode,
    NormalizationClass, ReplaceTextError, ReplaceTextOptions, ReplaceTextScope, plan_replace_text,
};
use stemma::{BlockNode, InlineNode, NodeId, RevisionInfo, TrackingStatus};

// ─── Fixtures (mirror spec_span_splice.rs) ───────────────────────────────────

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

fn opts(old: &str, new: &str) -> ReplaceTextOptions {
    ReplaceTextOptions {
        old: old.to_string(),
        new: new.to_string(),
        author: "replace-text-test".to_string(),
        scope: ReplaceTextScope::WholeDoc,
        expected: ExpectedMatches::Count(1),
        match_mode: MatchMode::Exact,
        on_barrier_match: BarrierPolicy::Skip,
    }
}

/// Plan + apply, returning the edited Document.
fn apply(doc: &Document, options: &ReplaceTextOptions) -> Result<Document, String> {
    let canonical = doc.snapshot().canonical.as_ref().clone();
    let plan = plan_replace_text(&canonical, options).map_err(|e| format!("{e:?}"))?;
    if plan.steps.is_empty() {
        return Err("empty plan".to_string());
    }
    let tx = EditTransaction {
        steps: plan.steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 0,
            identity: 0,
            author: Some(options.author.clone()),
            date: None,
            apply_op_id: None,
        },
    };
    doc.apply(&tx).map_err(|e| format!("{e:?}"))
}

fn first_block_id(doc: &Document) -> NodeId {
    doc.read().blocks[0].id.clone()
}

fn find_paragraph<'a>(doc: &'a Document, block_id: &NodeId) -> &'a stemma::ParagraphNode {
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && &p.id == block_id
        {
            return p;
        }
    }
    panic!("paragraph {block_id} not found");
}

/// Segments as `(status, author, text)` triples — the domain fingerprint.
fn segment_fingerprint(doc: &Document, block_id: &NodeId) -> Vec<(String, String, String)> {
    let para = find_paragraph(doc, block_id);
    para.segments
        .iter()
        .map(|seg| {
            let (status, author) = match &seg.status {
                TrackingStatus::Normal => ("normal".to_string(), String::new()),
                TrackingStatus::Inserted(r) => {
                    ("inserted".to_string(), r.author.clone().unwrap_or_default())
                }
                TrackingStatus::Deleted(r) => {
                    ("deleted".to_string(), r.author.clone().unwrap_or_default())
                }
                TrackingStatus::InsertedThenDeleted(sr) => (
                    "inserted_then_deleted".to_string(),
                    sr.inserted.author.clone().unwrap_or_default(),
                ),
            };
            let mut text = String::new();
            for inline in &seg.inlines {
                if let InlineNode::Text(t) = inline {
                    text.push_str(&t.text);
                }
            }
            (status, author, text)
        })
        .collect()
}

// ─── 1. Clean paragraph ──────────────────────────────────────────────────────

#[test]
fn replace_text_in_clean_paragraph() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>The quick brown fox.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let block_id = first_block_id(&doc);

    let edited = apply(&doc, &opts("quick", "slow")).expect("clean replace");
    assert_eq!(
        edited.read_accepted().expect("accept").read().blocks[0].text,
        "The slow brown fox."
    );
    assert_eq!(
        edited.read_rejected().expect("reject").read().blocks[0].text,
        "The quick brown fox."
    );
    // The change is attributed to the author.
    let fp = segment_fingerprint(&edited, &block_id);
    assert!(
        fp.iter()
            .any(|(s, a, _)| s == "inserted" && a == "replace-text-test"),
        "new text attributed to author: {fp:?}"
    );
}

// ─── 2. Match inside an already-tracked paragraph (the headline case) ────────

/// Body: "Payment due in " + <ins author=Stemma>"thirty "</ins> + "days." The
/// Normal lead region carries "Payment". replace_text("Payment","Fee") must
/// splice into the Normal region and carry Stemma's insertion through
/// untouched — the exact case replace_all REFUSES.
#[test]
fn replace_text_splices_through_a_tracked_paragraph() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Payment due in </w:t></w:r><w:ins w:id="1" w:author="Stemma" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">thirty </w:t></w:r></w:ins><w:r><w:t>days.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let block_id = first_block_id(&doc);

    let edited = apply(&doc, &opts("Payment", "Fee")).expect("splice through tracked paragraph");

    let fp = segment_fingerprint(&edited, &block_id);
    // Stemma's insertion survives byte/status/author-identical.
    assert!(
        fp.iter()
            .any(|(s, a, t)| s == "inserted" && a == "Stemma" && t == "thirty "),
        "the pre-existing Stemma insertion must survive untouched: {fp:?}"
    );
    // The new change is attributed to the editing author.
    assert!(
        fp.iter()
            .any(|(s, a, _)| s == "inserted" && a == "replace-text-test"),
        "the replacement is tracked under the editing author: {fp:?}"
    );
    // Accept-all: both changes land.
    assert_eq!(
        edited.read_accepted().expect("accept").read().blocks[0].text,
        "Fee due in thirty days."
    );
    // Reject-all: the document returns to its pre-anyone state.
    assert_eq!(
        edited.read_rejected().expect("reject").read().blocks[0].text,
        "Payment due in days."
    );
}

// ─── 3. expected_matches gate ────────────────────────────────────────────────

#[test]
fn zero_matches_fails_with_empty_site_list() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>nothing to see</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    let err = plan_replace_text(&canonical, &opts("absent", "x")).expect_err("zero matches");
    match err {
        ReplaceTextError::MatchCountMismatch { actual, sites, .. } => {
            assert_eq!(actual, 0);
            assert!(sites.is_empty());
        }
        other => panic!("expected MatchCountMismatch, got {other:?}"),
    }
}

#[test]
fn two_matches_with_default_expected_one_fails_with_contexts() {
    // Two paragraphs each containing "fee".
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>the fee is due</w:t></w:r></w:p><w:p><w:r><w:t>another fee here</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    let err = plan_replace_text(&canonical, &opts("fee", "charge")).expect_err("two matches");
    match err {
        ReplaceTextError::MatchCountMismatch { actual, sites, .. } => {
            assert_eq!(actual, 2, "both occurrences counted");
            assert_eq!(sites.len(), 2);
            // Each excerpt delimits the match.
            assert!(
                sites
                    .iter()
                    .all(|s| s.excerpt.contains('«') && s.excerpt.contains('»'))
            );
            // The two sites are in different blocks.
            assert_ne!(sites[0].block_id, sites[1].block_id);
        }
        other => panic!("expected MatchCountMismatch, got {other:?}"),
    }
}

#[test]
fn expected_all_replaces_every_occurrence() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>fee and fee and fee</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let mut options = opts("fee", "charge");
    options.expected = ExpectedMatches::All;
    let edited = apply(&doc, &options).expect("replace all");
    assert_eq!(
        edited.read_accepted().expect("accept").read().blocks[0].text,
        "charge and charge and charge"
    );
}

// ─── 4. normalize_ws ─────────────────────────────────────────────────────────

#[test]
fn normalize_ws_matches_nbsp_and_reports_whitespace() {
    // The paragraph contains a non-breaking space; the needle uses a plain space.
    let doc = Document::parse(&make_docx_with_body(
        "<w:p><w:r><w:t xml:space=\"preserve\">foo\u{00A0}bar baz</w:t></w:r></w:p>",
    ))
    .expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    let mut options = opts("foo bar", "QUX");
    options.match_mode = MatchMode::NormalizeWs;
    let plan = plan_replace_text(&canonical, &options).expect("normalize_ws match");
    assert_eq!(
        plan.matches.len(),
        1,
        "the nbsp text matches under normalize_ws"
    );
    assert!(
        plan.normalization_applied
            .contains(&NormalizationClass::Whitespace),
        "the receipt reports the whitespace folding fired: {:?}",
        plan.normalization_applied
    );

    // Exact mode: same input does NOT match (the nbsp is not a space).
    options.match_mode = MatchMode::Exact;
    let exact = plan_replace_text(&canonical, &options);
    assert!(
        matches!(
            exact,
            Err(ReplaceTextError::MatchCountMismatch { actual: 0, .. })
        ),
        "exact mode must not match across the nbsp"
    );
}

#[test]
fn normalize_ws_matches_curly_apostrophe_and_reports_it() {
    let doc = Document::parse(&make_docx_with_body(
        "<w:p><w:r><w:t>don\u{2019}t stop</w:t></w:r></w:p>",
    ))
    .expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    let mut options = opts("don't", "do not");
    options.match_mode = MatchMode::NormalizeWs;
    let plan = plan_replace_text(&canonical, &options).expect("curly apostrophe match");
    assert_eq!(plan.matches.len(), 1);
    assert!(
        plan.normalization_applied
            .contains(&NormalizationClass::Apostrophe),
        "the receipt reports the apostrophe folding fired: {:?}",
        plan.normalization_applied
    );
}

// ─── 5. Boundary refusal ─────────────────────────────────────────────────────

/// A needle straddling an opaque field anchor matches in NO single region. Under
/// `fail` the plan is refused; under `skip` that paragraph is reported as a
/// skipped straddle and contributes no step.
#[test]
fn match_straddling_an_anchor_is_skipped_or_failed() {
    // "see " + <field> + " end" — needle "see end" straddles the field.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> end</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();

    // Skip: no step, reported as a straddle, and NOT counted toward expected.
    let mut options = opts("see  end", "ref");
    options.expected = ExpectedMatches::All;
    options.on_barrier_match = BarrierPolicy::Skip;
    let plan = plan_replace_text(&canonical, &options).expect("skip straddle");
    assert!(plan.steps.is_empty(), "no step for a straddle under skip");
    assert_eq!(plan.skipped_straddles.len(), 1, "the straddle is reported");

    // Fail: the whole plan is refused.
    options.on_barrier_match = BarrierPolicy::Fail;
    let err = plan_replace_text(&canonical, &options).expect_err("fail straddle");
    assert!(
        matches!(err, ReplaceTextError::Engine(_)),
        "a straddle under fail refuses the plan: {err:?}"
    );
}

// ─── 6. Multiple matches in one region (single splice) ───────────────────────

#[test]
fn two_matches_in_one_region_apply_in_one_splice() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>cat and cat</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let block_id = first_block_id(&doc);
    let canonical = doc.snapshot().canonical.as_ref().clone();
    let mut options = opts("cat", "dog");
    options.expected = ExpectedMatches::All;
    let plan = plan_replace_text(&canonical, &options).expect("two-in-one-region");
    assert_eq!(
        plan.steps.len(),
        1,
        "one paragraph yields exactly one splice step"
    );

    let edited = apply(&doc, &options).expect("apply");
    assert_eq!(
        edited.read_accepted().expect("accept").read().blocks[0].text,
        "dog and dog"
    );
    let _ = block_id;
}

// ─── 7. Scope ────────────────────────────────────────────────────────────────

#[test]
fn single_block_scope_limits_the_search() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>fee one</w:t></w:r></w:p><w:p><w:r><w:t>fee two</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let ids: Vec<NodeId> = doc.read().blocks.iter().map(|b| b.id.clone()).collect();
    let canonical = doc.snapshot().canonical.as_ref().clone();

    let mut options = opts("fee", "charge");
    options.scope = ReplaceTextScope::SingleBlock(ids[1].clone());
    // Only one "fee" in the scoped block → default expected 1 succeeds.
    let plan = plan_replace_text(&canonical, &options).expect("scoped to one block");
    assert_eq!(plan.matches.len(), 1);
    assert_eq!(plan.matches[0].block_id, ids[1]);
}

// ─── 8. Output is a well-formed DOCX (the daily open-clean proxy) ────────────

/// The splice-through-tracked output must serialize to a VALID DOCX. This is the
/// daily-runnable proxy for "opens clean in Word": `replace_text` emits
/// `ReplaceSpanText` steps that ride the already-Word-certified `apply_span_splice`
/// materializer (spec_span_splice.rs), so it changes no materialization behavior;
/// a dedicated word-oracle run is unnecessary. We still gate the bytes here so
/// a regression that emits invalid OOXML is caught daily. (A `#[ignore]`d
/// word-oracle leg would be redundant with the certified splice path; the
/// validator gate is the standing check.)
#[test]
fn replace_text_output_through_tracked_paragraph_is_valid_docx() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Payment due in </w:t></w:r><w:ins w:id="1" w:author="Stemma" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">thirty </w:t></w:r></w:ins><w:r><w:t>days.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let edited = apply(&doc, &opts("Payment", "Fee")).expect("splice");

    // serialize() with default options gates at the Blocking validator level —
    // a successful serialize means the bytes are a well-formed package Word will
    // open. Then re-validate explicitly for the issue list.
    let bytes = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("replace_text output must serialize to valid DOCX bytes");
    let report = stemma::api::validate(&bytes);
    assert!(
        report.ok,
        "replace_text output must validate clean, issues: {:?}",
        report.issues
    );
}

// ─── 9. Body-only matching + zero-match diagnosis (three probes) ─────────────
//
// The benchmark's measured residual: bare zero-match errors with no "why" forced
// the agent back to read_block/apply_edit ceremony. The diagnosis adds, ONLY on a
// zero-match, an array of probe results — each firing only when it would change
// the outcome (no speculative advice; a genuinely absent needle yields []). The
// probes INFORM, never act.

/// A numbered heading "1.\tEvents" — import hoists "1." into literal_prefix, so
/// the body the matcher sees is "Events". The read view re-prepends the label,
/// so an agent reads "1.\tEvents".
fn numbered_heading_docx() -> Vec<u8> {
    make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">1.</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Events</w:t></w:r></w:p>"#,
    )
}

/// Pull the single firing diagnosis line, asserting exactly one probe fired.
fn one_diagnosis(err: ReplaceTextError) -> String {
    match err {
        ReplaceTextError::MatchCountMismatch {
            actual, diagnosis, ..
        } => {
            assert_eq!(actual, 0, "diagnosis is a zero-match feature");
            assert_eq!(
                diagnosis.len(),
                1,
                "expected exactly one probe to fire: {diagnosis:?}"
            );
            diagnosis.into_iter().next().unwrap()
        }
        other => panic!("expected MatchCountMismatch, got {other:?}"),
    }
}

/// PROBE (b) — label-strip. A needle that includes the structural numbering label
/// ("1.\tEvents") finds zero matches (the label lives in literal_prefix, not the
/// runs), and the diagnosis names the label and the working label-stripped needle.
#[test]
fn zero_match_label_probe_teaches_dropping_the_label() {
    let doc = Document::parse(&numbered_heading_docx()).expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();

    let err = plan_replace_text(&canonical, &opts("1.\tEvents", "Meetings"))
        .expect_err("a needle including the numbering label matches nothing");
    let d = one_diagnosis(err);
    assert!(
        d.contains("1.") && d.contains("Events") && d.contains("numbering label"),
        "the label probe names the structural label and the working needle: {d}"
    );

    // The advice works: the label-stripped needle matches normally.
    let plan = plan_replace_text(&canonical, &opts("Events", "Meetings"))
        .expect("the label-stripped needle matches the body");
    assert_eq!(plan.matches.len(), 1, "'Events' matches the body once");
}

/// PROBE (a) — normalize_ws. A needle that differs from the body only by an nbsp
/// finds zero EXACT matches, and the diagnosis names the equivalence class and
/// suggests match_mode normalize_ws.
#[test]
fn zero_match_normalize_ws_probe_names_the_classes() {
    let doc = Document::parse(&make_docx_with_body(
        "<w:p><w:r><w:t xml:space=\"preserve\">foo\u{00A0}bar baz</w:t></w:r></w:p>",
    ))
    .expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    // Exact-mode needle "foo bar" (plain space) vs the nbsp in the doc → 0 matches.
    let err = plan_replace_text(&canonical, &opts("foo bar", "QUX"))
        .expect_err("exact mode does not match across the nbsp");
    let d = one_diagnosis(err);
    assert!(
        d.contains("whitespace") && d.contains("normalize_ws"),
        "the normalize_ws probe names the class and the mode: {d}"
    );
}

/// PROBE (c) — tracked-wall straddle. A needle present across a paragraph's Normal
/// text but spanning an opaque anchor finds zero region-local matches, and the
/// diagnosis names the block and the wall.
#[test]
fn zero_match_straddle_probe_names_the_block_and_wall() {
    // "see " + <field> + " end" — needle "see  end" straddles the field.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> end</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    let err = plan_replace_text(&canonical, &opts("see  end", "ref"))
        .expect_err("a needle straddling the field matches no single region");
    let d = one_diagnosis(err);
    assert!(
        d.contains("wall") && (d.contains("p_") || d.contains("block")),
        "the straddle probe names the wall and the block: {d}"
    );
}

/// PROBE (4) — already-applied. The needle is absent because a prior call already
/// replaced it: the REPLACEMENT text sits where the needle should be. The
/// diagnosis names the replacement and the block and says the change appears
/// already applied (the idempotency signature).
#[test]
fn zero_match_already_applied_probe_reports_the_duplicate() {
    // The doc already reads "charge" — replace_text("fee","charge") finds no "fee"
    // but "charge" is already present.
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>the charge is due</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    let err = plan_replace_text(&canonical, &opts("fee", "charge"))
        .expect_err("the needle 'fee' is absent — already replaced by 'charge'");
    let d = one_diagnosis(err);
    assert!(
        d.contains("charge") && d.contains("already applied"),
        "the already-applied probe names the replacement and the signature: {d}"
    );
}

/// PROBE (5) — out-of-scope. The needle matches nothing INSIDE the given scope but
/// exists outside it. The diagnosis names how many matches are outside and tells
/// the agent to widen the scope (rather than concluding the phrase is absent).
#[test]
fn zero_match_out_of_scope_probe_points_outside_the_scope() {
    // Block 0 has no "fee"; block 1 does. Scope to block 0 → zero in scope, but a
    // whole-body scan finds the match in block 1.
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>nothing here</w:t></w:r></w:p><w:p><w:r><w:t>the fee is due</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let ids: Vec<NodeId> = doc.read().blocks.iter().map(|b| b.id.clone()).collect();
    let canonical = doc.snapshot().canonical.as_ref().clone();

    let mut options = opts("fee", "charge");
    options.scope = ReplaceTextScope::SingleBlock(ids[0].clone());
    let err = plan_replace_text(&canonical, &options)
        .expect_err("'fee' is absent from the scoped block 0");
    let d = one_diagnosis(err);
    assert!(
        d.contains("OUTSIDE") && d.contains("widen"),
        "the out-of-scope probe reports matches outside the scope and says to widen it: {d}"
    );
}

/// THE GENERAL nearest-candidate reporter — the base mechanism the classifications
/// layer on. A typo'd needle matches no classification, but the document has a
/// near miss; the reporter names the closest substring and the first concrete
/// difference. This catches the failure class the classifications don't model.
#[test]
fn zero_match_nearest_candidate_reports_a_typo() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>the agreement is binding</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    // "agreemant" (one letter wrong) matches nothing exactly; the document has
    // "agreement" one char away — no classification applies, the base speaks.
    let err = plan_replace_text(&canonical, &opts("agreemant", "contract"))
        .expect_err("the typo'd needle matches nothing");
    let d = one_diagnosis(err);
    assert!(
        d.contains("near miss") && d.contains("offset"),
        "the nearest-candidate reporter names the near miss and the offset: {d}"
    );
}

/// No speculative advice: a genuinely-absent needle (no near candidate, no
/// out-of-scope match, replacement not present) yields an EMPTY diagnosis — no
/// classification and not the base fire when none would change the outcome.
#[test]
fn zero_match_genuinely_absent_needle_has_empty_diagnosis() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>nothing to see here</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let canonical = doc.snapshot().canonical.as_ref().clone();
    // "xyzzyx" shares no near substring with the body, so even the general
    // nearest-candidate reporter stays silent.
    let err = plan_replace_text(&canonical, &opts("xyzzyx", "q")).expect_err("zero matches");
    match err {
        ReplaceTextError::MatchCountMismatch {
            actual, diagnosis, ..
        } => {
            assert_eq!(actual, 0);
            assert!(
                diagnosis.is_empty(),
                "a genuinely-absent needle gets no speculative diagnosis: {diagnosis:?}"
            );
        }
        other => panic!("expected MatchCountMismatch, got {other:?}"),
    }
}
