//! Blindspot regression: `SetRunFormatting` must put EXACTLY the requested mark
//! on the targeted span — for the four marks the asserting tests never pin.
//!
//! `tests/run_formatting.rs` only ever requests `bold` (and the value-bearing
//! caps/small_caps/color/font in `edit_fidelity_invariants.rs`). The four
//! remaining boolean marks — `underline`, `strike`, `subscript`, `superscript`
//! — are wired in `run_formatting.rs::apply_marks` (lines 330-341) but are never
//! requested through the verb in any asserting test. A wiring swap
//! (subscript -> superscript) or a silent strike drop would pass both tiers.
//!
//! DOMAIN-CORRECT BEHAVIOR (ECMA-376 §17.3.2): toggling one run property ON must
//! produce exactly THAT property on the targeted run, not a sibling:
//!   - underline   -> w:u           (§17.3.2.40)  view TextMark::Underline
//!   - strike      -> w:strike      (§17.3.2.37)  view TextMark::Strike
//!   - subscript   -> w:vertAlign=subscript   (§17.3.2.42) view TextMark::Subscript
//!   - superscript -> w:vertAlign=superscript (§17.3.2.42) view TextMark::Superscript
//!
//! After accept-all the targeted span carries exactly that one mark; reject-all
//! removes it.

use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::{accept_all, reject_all_with_styles};

// ─── Fixture (paste from edit_fidelity_invariants.rs:29-62) ──────────────────

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

fn doc_and_ids(paragraphs: &[&str]) -> (CanonDoc, Vec<String>) {
    let doc = Document::parse(&make_test_docx(paragraphs)).expect("parse");
    let ids = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();
    ((*doc.snapshot().canonical).clone(), ids)
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Gate".to_string()),
            date: Some("2026-06-06T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

// ─── Helper: the run-property fingerprint of the targeted IR run ─────────────
//
// `SetRunFormatting` splits the matched span into its own `TextNode`. We inspect
// that node DIRECTLY (not the comprehension view, which coalesces adjacent
// same-formatting runs and so cannot isolate a single split span). The domain
// properties live in two places in the IR:
//   - underline / subscript / superscript -> `TextNode.marks` (Mark enum)
//   - strike                              -> `TextNode.style_props.strike` tri-state
// Bold/Italic (Mark) and Caps/SmallCaps (tri-state) are the sibling axes that a
// wiring swap could light up by mistake; we include them so "exactly one" is
// enforced against every neighbour.

/// A stable, ordered fingerprint of the run-level formatting properties this
/// verb can toggle. Returns the set of property tags that are ON.
fn run_props(t: &TextNode) -> Vec<&'static str> {
    let mut out = Vec::new();
    if t.marks.contains(&Mark::Bold) {
        out.push("bold");
    }
    if t.marks.contains(&Mark::Italic) {
        out.push("italic");
    }
    if t.marks.contains(&Mark::Underline) {
        out.push("underline");
    }
    if t.marks.contains(&Mark::Subscript) {
        out.push("subscript");
    }
    if t.marks.contains(&Mark::Superscript) {
        out.push("superscript");
    }
    if t.style_props.strike == MarkValue::On {
        out.push("strike");
    }
    if t.style_props.caps == MarkValue::On {
        out.push("caps");
    }
    if t.style_props.small_caps == MarkValue::On {
        out.push("small_caps");
    }
    out
}

/// The first `TextNode` whose text equals `target` across all paragraph blocks.
fn run_with_text(canon: &CanonDoc, target: &str) -> Option<TextNode> {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline
                        && t.text == target
                    {
                        return Some((**t).clone());
                    }
                }
            }
        }
    }
    None
}

fn props_on_span(canon: &CanonDoc, target: &str) -> Vec<&'static str> {
    run_props(
        &run_with_text(canon, target).unwrap_or_else(|| panic!("no IR run with text '{target}'")),
    )
}

// ─── Per-mark cases ──────────────────────────────────────────────────────────
//
// One InlineMarkSet toggling exactly one boolean ON; everything else default.
// Domain rule: the targeted span carries exactly that property after accept-all,
// and NONE of the sibling axes (no swap, no extra). reject-all restores the
// unmarked span.

/// Apply `marks` to the word "Format" in "Format me" as a tracked change, then
/// assert: tracked-live carries exactly `expected`, reject-all clears it,
/// accept-all keeps exactly `expected`.
fn assert_single_mark(label: &str, marks: InlineMarkSet, expected: &'static str) {
    let (base, ids) = doc_and_ids(&["Format me"]);
    let steps = vec![EditStep::SetRunFormatting {
        block_id: NodeId::from(ids[0].as_str()),
        expect: "Format".to_string(),
        semantic_hash: None,
        marks,
        style: RunStyleEdit::default(),
        rationale: None,
    }];

    // Base has no properties on the (whole) run.
    assert!(
        props_on_span(&base, "Format me").is_empty(),
        "[{label}] base run must start unmarked"
    );

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .unwrap_or_else(|e| panic!("[{label}] tracked apply failed: {e}"))
    .0;

    // The targeted span "Format" carries EXACTLY the expected property — not a
    // sibling, and no extras.
    let live = props_on_span(&tracked, "Format");
    assert_eq!(
        live,
        vec![expected],
        "[{label}] tracked span must carry exactly [{expected:?}], got {live:?}"
    );

    // Accept-all keeps exactly that property.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let accepted_props = props_on_span(&accepted, "Format");
    assert_eq!(
        accepted_props,
        vec![expected],
        "[{label}] accept-all span must carry exactly [{expected:?}], got {accepted_props:?}"
    );

    // Reject-all removes the property entirely; the span is unmarked again.
    let mut rejected = tracked;
    reject_all_with_styles(&mut rejected, None);
    let rejected_props = props_on_span(&rejected, "Format");
    assert!(
        rejected_props.is_empty(),
        "[{label}] reject-all must remove the property, got {rejected_props:?}"
    );
}

#[test]
fn underline_sets_exactly_underline() {
    assert_single_mark(
        "underline",
        InlineMarkSet {
            underline: true,
            ..Default::default()
        },
        "underline",
    );
}

#[test]
fn strike_sets_exactly_strike() {
    assert_single_mark(
        "strike",
        InlineMarkSet {
            strike: true,
            ..Default::default()
        },
        "strike",
    );
}

#[test]
fn subscript_sets_exactly_subscript() {
    assert_single_mark(
        "subscript",
        InlineMarkSet {
            subscript: true,
            ..Default::default()
        },
        "subscript",
    );
}

#[test]
fn superscript_sets_exactly_superscript() {
    assert_single_mark(
        "superscript",
        InlineMarkSet {
            superscript: true,
            ..Default::default()
        },
        "superscript",
    );
}

// ─── reject_all persisted-markup check ────────────────────────────────────
//
// The tests above (`assert_single_mark`) already exercised `reject_all` and
// passed BEFORE the class-audit fix (spec_selective_formatting_resolution.rs)
// — but only by checking the IN-MEMORY canonical model via `props_on_span`,
// never a real save+reparse round trip. `project_block_for_accept_reject`
// (the shared function `accept_all`/`reject_all` and the read_accepted/
// read_rejected projections all route through) calls the SAME
// `reject_text_formatting` helper the selective (by-id) path uses — so it
// carried the identical latent bug (rpr_authored never restored, so the
// serializer kept re-emitting the reverted mark) with no test catching it,
// simply because nothing here serialized. This closes that gap for the
// blanket path specifically, confirming the fix is shared, not duplicated.
#[test]
fn reject_all_persists_the_reverted_mark_after_a_save() {
    let doc = Document::parse(&make_test_docx(&["Format me"])).expect("parse");
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text == "Format me")
        .expect("target");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::SetRunFormatting {
                block_id: target.id.clone(),
                expect: "Format".to_string(),
                semantic_hash: Some(target.guard.clone()),
                marks: InlineMarkSet {
                    bold: true,
                    ..Default::default()
                },
                style: RunStyleEdit::default(),
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                author: Some("Gate".to_string()),
                date: Some("2026-06-06T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("tracked SetRunFormatting applies");

    let rejected = doc
        .project(stemma::Resolution::RejectAll)
        .expect("reject-all projects");
    assert!(
        props_on_span(&rejected.snapshot().canonical, "Format").is_empty(),
        "reject_all must clear bold in memory"
    );

    // The persisted half: serialize the ALREADY-rejected doc (an ordinary
    // save, nothing pending), then re-parse and check again. THE BUG kept
    // `<w:b/>` in the output despite `marks` being empty in memory.
    let bytes = rejected
        .serialize(&stemma::ExportOptions::unchecked())
        .expect("serialize the rejected doc");
    let reparsed = Document::parse(&bytes).expect("re-parse");
    let reparsed_props = props_on_span(&reparsed.snapshot().canonical, "Format");
    assert!(
        reparsed_props.is_empty(),
        "reject_all's reverted (unformatted) span must survive a save + \
         re-parse — THE BUG kept bold in the saved file: {reparsed_props:?}"
    );
}

/// AGREEMENT: `read_rejected` (the markdown PROJECTION) and a save+reparse
/// of the identical resolution (the PERSISTED path) must describe the same
/// outcome. `Document::read_rejected` is literally
/// `self.project(Resolution::RejectAll)` (api.rs) — the SAME call
/// `reject_all` makes — so there was never a separate "projections" code
/// path to diverge from; both consumers share one resolution function
/// (`project_block_for_accept_reject` -> `reject_text_formatting`). What
/// COULD diverge (and, pre-fix, did) is what each consumer reads off the
/// resolved model afterward: the markdown renderer reads `marks` directly;
/// the OOXML serializer separately reads `rpr_authored`. Now that
/// `reject_text_formatting` restores both in lockstep from the same
/// `FormattingChange` record, no consumer can see a different answer than
/// any other — this test is the standing proof, not just an assertion.
#[test]
fn read_rejected_markdown_and_a_persisted_save_agree_on_the_reverted_mark() {
    let doc = Document::parse(&make_test_docx(&["Format me"])).expect("parse");
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text == "Format me")
        .expect("target");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::SetRunFormatting {
                block_id: target.id.clone(),
                expect: "Format".to_string(),
                semantic_hash: Some(target.guard.clone()),
                marks: InlineMarkSet {
                    bold: true,
                    ..Default::default()
                },
                style: RunStyleEdit::default(),
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                author: Some("Gate".to_string()),
                date: Some("2026-06-06T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("tracked SetRunFormatting applies");

    // The PROJECTION.
    let rejected_view = doc.read_rejected().expect("read_rejected");
    let markdown = rejected_view.to_markdown();
    let projection_says_bold = markdown.contains("<b>Format</b>");

    // The PERSISTED path: the SAME projection, then saved and re-parsed.
    let bytes = rejected_view
        .serialize(&stemma::ExportOptions::unchecked())
        .expect("serialize");
    let reparsed = Document::parse(&bytes).expect("re-parse");
    let persisted_says_bold = !props_on_span(&reparsed.snapshot().canonical, "Format").is_empty();

    assert!(
        !projection_says_bold && !persisted_says_bold,
        "both consumers must agree the mark was reverted: \
         projection_says_bold={projection_says_bold}, persisted_says_bold={persisted_says_bold}"
    );
    assert_eq!(
        projection_says_bold, persisted_says_bold,
        "THE DIVERGENCE THIS FIX CLOSED: projection and persisted output must \
         never disagree about the same resolution's outcome"
    );
}
