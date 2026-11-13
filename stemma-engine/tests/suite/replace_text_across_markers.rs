//! `replace_text` (the worklist's find/replace) matches across zero-width
//! markers, and a refusal names the wall it actually stopped at.
//!
//! Domain rules:
//! - A bookmark boundary (§17.13.6.2), a comment range boundary (§17.13.4.4)
//!   and a permission range boundary (§17.13.7.2) hold no text: the reader sees
//!   one unbroken run of text across them, and Word's own Find matches across
//!   them. So does `replace_text`, as it already did across proofing marks.
//! - The edit changes only the text it rewrites: each marker keeps its offset
//!   in the text, exactly as a whole-paragraph replace of the same paragraph
//!   places it, and Reject All reproduces the source.
//! - When an occurrence does cross something the splice cannot carry, the
//!   refusal names that thing (the inline object by id, the pending deletion
//!   by author), never a list of kinds that are not there.
//!
//! Daily tier: synthetic in-memory DOCX.

use stemma::api::Document;
use stemma::edit::{
    BarrierPolicy, EditError, EditTransaction, ExpectedMatches, MatchMode, MaterializationMode,
    ReplaceTextError, ReplaceTextOptions, ReplaceTextScope, plan_replace_text,
};
use stemma::{ExportOptions, Resolution, RevisionInfo};

use crate::common::{docx_with_body_and_comments, marker_stream, read_zip_entry};

const DATE: &str = r#"w:date="2026-10-01T09:00:00Z""#;

fn options(old: &str, new: &str) -> ReplaceTextOptions {
    ReplaceTextOptions {
        old: old.to_string(),
        new: new.to_string(),
        author: "Reviewer".to_string(),
        scope: ReplaceTextScope::BodyAndTables,
        expected: ExpectedMatches::Count(1),
        match_mode: MatchMode::Exact,
        on_barrier_match: BarrierPolicy::Fail,
    }
}

fn plan(doc: &Document, old: &str, new: &str) -> Result<EditTransaction, ReplaceTextError> {
    let plan = plan_replace_text(&doc.snapshot().canonical, &options(old, new))?;
    Ok(EditTransaction {
        steps: plan.steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 0,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: Some("2026-11-05T19:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
}

fn replace(doc: &Document, old: &str, new: &str) -> Document {
    let transaction = plan(doc, old, new).expect("the occurrence is replaceable");
    doc.apply(&transaction).expect("apply")
}

fn stream(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    marker_stream(&read_zip_entry(&bytes, "word/document.xml").expect("document.xml"))
}

/// The same edit as a whole-paragraph replace of the first paragraph: the
/// reference placement of every marker.
fn whole_paragraph_replace(doc: &Document, text: &str) -> Document {
    let block = &doc.read().blocks[0];
    let json = serde_json::json!({
        "ops": [{"op": "replace", "target": block.id.to_string(), "expect": block.text,
            "content": {"type": "paragraph", "content": [{"type": "text", "text": text}]}}],
        "revision": {"author": "Reviewer", "date": "2026-11-05T19:00:00Z"},
    })
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .expect("whole-paragraph replace")
}

/// Replace `old` with `new`; check the redline, that it places the markers
/// as the whole-paragraph replace to `whole` does, that Reject All restores
/// the source, and that the result survives save and reopen.
fn assert_replaced(source: &Document, old: &str, new: &str, whole: &str, redline: &str) {
    let edited = replace(source, old, new);
    assert_eq!(stream(&edited), redline, "redline");
    assert_eq!(
        stream(&whole_paragraph_replace(source, whole)),
        redline,
        "markers sit where the whole-paragraph replace puts them"
    );
    assert_eq!(
        stream(&edited.project(Resolution::RejectAll).unwrap()),
        stream(source),
        "Reject All restores the source"
    );
    let reopened =
        Document::parse(&edited.serialize(&ExportOptions::default()).unwrap()).expect("reopen");
    assert_eq!(stream(&reopened), redline, "save and reopen");
}

#[test]
fn an_occurrence_across_a_bookmark_range_is_replaced() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">The Seller shall </w:t></w:r><w:bookmarkStart w:id="0" w:name="terms"/><w:r><w:t xml:space="preserve">deliver the </w:t></w:r><w:bookmarkEnd w:id="0"/><w:r><w:t>goods promptly.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    assert_replaced(
        &source,
        "shall deliver the goods",
        "must ship the goods",
        "The Seller must ship the goods promptly.",
        "The Seller <-shall {bs0}deliver-><+must ship+> the {be0}goods promptly.|",
    );
}

#[test]
fn an_occurrence_across_a_point_bookmark_is_replaced() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Both </w:t></w:r><w:bookmarkStart w:id="2" w:name="point"/><w:bookmarkEnd w:id="2"/><w:r><w:t>parties agree.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    assert_replaced(
        &source,
        "Both parties",
        "All parties",
        "All parties agree.",
        "<-Both-><+All+> {bs2}{be2}parties agree.|",
    );
}

#[test]
fn an_occurrence_across_a_comment_range_is_replaced() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">The fee is </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t xml:space="preserve">due monthly </w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:t>in advance.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &["0"])).unwrap();
    assert_replaced(
        &source,
        "is due monthly",
        "is payable monthly",
        "The fee is payable monthly in advance.",
        "The fee is {cs0}<-due-><+payable+> monthly {ce0}{cr0}in advance.|",
    );
}

#[test]
fn an_occurrence_across_a_permission_range_is_replaced() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">The Buyer </w:t></w:r><w:permStart w:id="1" w:edGrp="everyone"/><w:r><w:t xml:space="preserve">shall pay </w:t></w:r><w:permEnd w:id="1"/><w:r><w:t>on time.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    assert_replaced(
        &source,
        "Buyer shall pay on",
        "Buyer pays on",
        "The Buyer pays on time.",
        "The Buyer {ps1}<-shall pay-><+pays+> {pe1}on time.|",
    );
}

/// An untracked edit through the same splice keeps a marker that stood in the
/// removed text at the point of removal.
#[test]
fn a_direct_replace_keeps_a_marker_from_the_removed_text() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">The Seller shall </w:t></w:r><w:bookmarkStart w:id="0" w:name="terms"/><w:r><w:t xml:space="preserve">deliver the </w:t></w:r><w:bookmarkEnd w:id="0"/><w:r><w:t>goods promptly.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    let mut transaction = plan(&source, "shall deliver the goods", "must ship the goods").unwrap();
    transaction.materialization_mode = MaterializationMode::Direct;
    let edited = source.apply(&transaction).expect("direct apply");
    assert_eq!(
        stream(&edited),
        "The Seller {bs0}must ship the {be0}goods promptly.|"
    );
}

fn straddle_walls(doc: &Document, old: &str) -> (Vec<String>, String) {
    match plan(doc, old, "x") {
        Err(ReplaceTextError::Engine(error @ EditError::FindReplaceBarrierStraddle { .. })) => {
            let message = error.to_string();
            let EditError::FindReplaceBarrierStraddle { walls, .. } = error else {
                unreachable!()
            };
            (walls, message)
        }
        Err(other) => panic!("expected a straddle refusal, got {other:?}"),
        Ok(_) => panic!("{old:?} straddles a wall and must be refused"),
    }
}

#[test]
fn a_straddle_names_the_pending_deletion_it_crosses() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Pay the </w:t></w:r><w:del w:id="1" w:author="Ann" {DATE}><w:r><w:delText xml:space="preserve">full </w:delText></w:r></w:del><w:r><w:t>fee.</w:t></w:r></w:p>"#
    );
    let source = Document::parse(&docx_with_body_and_comments(&body, &[])).unwrap();
    let (walls, message) = straddle_walls(&source, "the fee");
    assert_eq!(walls, ["a pending deletion by Ann"]);
    assert!(
        !message.contains("hyperlink") && !message.contains("field"),
        "the message names only what is there: {message}"
    );
}

#[test]
fn a_straddle_names_the_inline_object_it_crosses() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> end</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    let field = source.read().blocks[0]
        .segments
        .iter()
        .find_map(|segment| match segment {
            stemma::view::SegmentView::Opaque { id, .. } => Some(id.to_string()),
            _ => None,
        })
        .expect("the field is an inline object");
    let (walls, _) = straddle_walls(&source, "see  end");
    assert_eq!(walls, [format!("field '{field}'")]);
}

/// Word's proofing marks bracket the word it flagged (`w:proofErr`,
/// §17.13.8.1) and hug it as a move's delimiters hug the moved text: a word
/// an edit adds right before the bracket is not part of the flagged word, so
/// it lands outside the bracket.
#[test]
fn text_added_before_a_proofing_bracket_stays_outside_it() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Closing note. </w:t></w:r><w:proofErr w:type="spellStart"/><w:r><w:t>Recieved</w:t></w:r><w:proofErr w:type="spellEnd"/><w:r><w:t xml:space="preserve"> today.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    let edited = whole_paragraph_replace(&source, "Closing note. Kindly Recieved today.");
    let bytes = edited.serialize(&ExportOptions::default()).unwrap();
    let xml = read_zip_entry(&bytes, "word/document.xml").unwrap();
    let position = |needle: &str| {
        xml.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {xml}"))
    };
    let added = position("Kindly");
    let start = position(r#"w:type="spellStart""#);
    let word = position(">Recieved<");
    let end = position(r#"w:type="spellEnd""#);
    assert!(added < start && start < word && word < end, "{xml}");
}

/// A foreign-namespace element is foreign markup whatever its name. One whose
/// name starts with "move" is no WordprocessingML move marker (§17.13.5): the
/// paragraph holds no move, a whole-paragraph replace is an ordinary edit, and
/// the element is still in the file afterwards.
#[test]
fn a_foreign_element_named_like_a_move_marker_is_not_a_move() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">The Seller shall </w:t></w:r><acme:moveMarker xmlns:acme="urn:example:acme"/><w:r><w:t>deliver the goods.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    let edited = whole_paragraph_replace(&source, "The Buyer shall collect the goods.");
    assert_eq!(
        edited.project(Resolution::AcceptAll).unwrap().read().blocks[0].text,
        "The Buyer shall collect the goods."
    );
    assert_eq!(
        edited.project(Resolution::RejectAll).unwrap().read().blocks[0].text,
        "The Seller shall deliver the goods."
    );
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = read_zip_entry(&bytes, "word/document.xml").unwrap();
    assert!(xml.contains("acme:moveMarker"), "{xml}");
}
