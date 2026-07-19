//! REGRESSION TESTS (formerly `#[ignore]`d known-bug docs — fixed by the
//! authored-absolute tab-stop model): paragraph
//! tab-stop re-emission on the edit path.
//!
//! ROOT-CAUSE FAMILY (historical): `ParagraphNode.tab_stops` used to store
//! positions body-left-relative and resolved-effective; the serializer
//! re-added a synthetic absolute prefix stop next to those relative values and
//! deduped the collisions (bug 1), and the parser clamped authored positions
//! to ±31680 twips, colliding boundary stops (bug 2).
//!
//! THE FIX: `tab_stops` now holds the AUTHORED direct pPr stops verbatim
//! (page-absolute, unclamped) and the serializer re-emits exactly that; the
//! derived view value lives separately in `effective_tab_stops_rel`.

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};

fn reserialize_trigger() -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetDocDefaults {
            font_family: Some("Calibri".to_string()),
            font_size_half_points: None,
            rationale: Some("tab-stop reemit reserialize trigger".to_string()),
        }],
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: Some("reserialize trigger".to_string()),
    }
}

fn edited_document_xml(fixture: &str) -> String {
    let bytes = std::fs::read(fixture).expect("read fixture");
    let doc = Document::parse(&bytes).expect("parse");
    let edited = doc.apply(&reserialize_trigger()).expect("apply trigger");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = stemma::docx::DocxArchive::read(&out).expect("read output");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf-8")
}

/// Bug 1 — relative/absolute collision on literal-prefix paragraphs.
/// Fixture: "(a)\tBody…" with explicit stops at 360 and 720. Import stores the
/// 720 stop as 360-relative (body_left = 360); serialize emits it at absolute
/// 360, colliding with the synthetic consumed-prefix stop, and dedup drops it.
/// DOMAIN RULE: both authored stops re-emit at their authored positions.
#[test]
fn prefix_paragraph_keeps_both_authored_tab_stops() {
    let xml = edited_document_xml(
        "testdata/spec-compliance/indent-interaction-audit/multi-tab-prefix-skip/input.docx",
    );
    let first_para = &xml[xml.find("<w:p>").unwrap_or(0)..];
    let first_para = &first_para[..first_para.find("</w:p>").unwrap_or(first_para.len())];
    assert!(
        first_para.contains(r#"w:pos="360""#) && first_para.contains(r#"w:pos="720""#),
        "both authored stops (360 and 720) must survive the rebuild; \
         first paragraph: {first_para}"
    );
}

/// Bug 2 — position collapse / alignment corruption near the 22-inch
/// boundary (31680 twips). Fixture stops: 1440 left, 31680 right, 32000 left.
/// Output loses the 32000 stop and flips the surviving 31680 stop to left.
/// DOMAIN RULE: authored positions and alignments re-emit verbatim; if a
/// position is out of the supported range, that is a loud refusal, not a
/// silent rewrite (CLAUDE.md no-silent-fallbacks).
#[test]
fn tab_stops_near_word_position_limit_survive_verbatim() {
    let xml =
        edited_document_xml("testdata/spec-compliance/ms-paragraph-props/tab-pos-range/input.docx");
    assert!(
        xml.contains(r#"w:pos="32000""#),
        "the authored stop at 32000 must survive; document.xml: {xml}"
    );
    assert!(
        xml.contains(r#"w:val="right""#),
        "the authored right-aligned stop must keep its alignment; document.xml: {xml}"
    );
}
