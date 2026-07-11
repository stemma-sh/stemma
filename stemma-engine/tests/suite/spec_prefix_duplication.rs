//! Prefix-duplication contract: a write whose content begins with a prefix that
//! duplicates the paragraph's `literal_prefix` numbering label is REFUSED — the
//! SAME way on the whole-paragraph replace path and the span-splice path.
//!
//! Domain rule (CLAUDE.md "No silent fallbacks"): a `literal_prefix` is the
//! canonical home of a paragraph's typed-in enumeration label ("1.", "(a)"),
//! hoisted out of the runs by import and re-emitted as real text by the
//! serializer. When an agent — who reads the label re-prepended in the view as
//! "1.\tEvents" — echoes that label back in a write, the two write paths used to
//! disagree:
//!   - whole-paragraph replace SILENTLY stripped the echoed label
//!     (`strip_duplicated_numbering_prefix`), so the label vanished without a
//!     trace and the agent never learned the numbering was already present;
//!   - span splice had NO guard at all, so the echoed "1.\t" was inserted into
//!     the body and the SAVED document showed doubled numbering ("1.1.Events").
//!
//! One protected-but-silent path, one unprotected-and-corrupting path. The
//! contract makes them identical and loud: refuse with an error that names the
//! duplicated label and what the paragraph already reads, so the agent fixes the
//! op by omitting the label. (Per task-1 review: this error is "the single most
//! valuable string in the fix" — it breaks the doubling chain at step one.)
//!
//! A DIFFERENT label at the head (an attempted in-text renumber) is ALSO
//! refused — the paragraph's own label is re-emitted untouched, so the result
//! would stack ("1.2.\tEvents"). Its refusal message is intent-preserving: it
//! names both labels and says label changes via text replace are unsupported
//! (never "just omit it", which would silently abandon the renumber).
//!
//! Legitimate cases that must NOT be refused (no overreach):
//!   - a paragraph WITHOUT numbering keeps a literal "1.\t" as real content;
//!   - editing text that doesn't start at the paragraph head is unaffected.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    ResolvedSpanSelector,
};
use stemma::{BlockNode, InlineNode, NodeId, RevisionInfo};

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

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 1,
        author: Some("prefix-test".to_string()),
        date: Some("2026-01-01T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn text_content(s: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(s.to_string())],
    }
}

fn apply_steps(doc: &Document, steps: Vec<EditStep>) -> Result<Document, stemma::RuntimeError> {
    doc.apply(&EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    })
}

fn first_block_id_and_guard(doc: &Document) -> (NodeId, String) {
    let view = doc.read();
    (view.blocks[0].id.clone(), view.blocks[0].guard.clone())
}

fn handle_of_span(doc: &Document, text: &str) -> String {
    let view = doc.read();
    view.blocks[0]
        .segments
        .iter()
        .find_map(|s| match s {
            stemma::view::SegmentView::Text {
                text: t, handle, ..
            } if t == text => handle.clone(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no span with text {text:?}"))
        .0
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

/// Full visible text of the accepted document's first block: the
/// `literal_prefix` label (re-emitted by the serializer) followed by every
/// run's text. This is what Word would render.
fn accepted_full_text(doc: &Document, block_id: &NodeId) -> String {
    let accepted = doc.read_accepted().expect("accept");
    let para = find_paragraph(&accepted, block_id);
    let mut text = para.literal_prefix.clone().unwrap_or_default();
    for seg in &para.segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                text.push_str(&t.text);
            }
        }
    }
    text
}

/// Body "1.\tEvents" — a heading whose "1." import hoists into `literal_prefix`
/// (the body starts uppercase, satisfying the prefix detector). The read view
/// re-prepends the label, so an agent sees "1.\tEvents".
fn numbered_heading_docx() -> Vec<u8> {
    make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">1.</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Events</w:t></w:r></w:p>"#,
    )
}

/// Assert the import hoisted "1." into literal_prefix (the precondition that
/// makes the duplication possible). If this ever stops holding the fixture is
/// wrong, not the code.
fn assert_literal_prefix_present(doc: &Document, block_id: &NodeId) {
    let para = find_paragraph(doc, block_id);
    assert!(
        para.literal_prefix.as_deref() == Some("1."),
        "fixture precondition: import must hoist '1.' into literal_prefix, got {:?}",
        para.literal_prefix
    );
}

// ─── The bug, as a contract: both paths refuse the duplicated prefix ─────────

/// SPAN PATH (the one that shipped "1.1.Events"): a span splice replacing the
/// body "Events" with content that re-includes the "1.\t" label must be REFUSED.
/// Before the guard, this silently doubled the numbering in the saved output.
#[test]
fn span_splice_duplicating_literal_prefix_is_refused() {
    let doc = Document::parse(&numbered_heading_docx()).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    assert_literal_prefix_present(&doc, &block_id);

    let events_handle = handle_of_span(&doc, "Events");
    let result = apply_steps(
        &doc,
        vec![EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(events_handle),
            // The agent re-types the label it saw in the view.
            content: text_content("1.\tEvents"),
            rationale: None,
        }],
    );

    let err = match result {
        Ok(applied) => {
            // If it didn't refuse, prove the corruption it let through, so a
            // future regression has a precise failure message.
            let text = accepted_full_text(&applied, &block_id);
            panic!(
                "span splice duplicating the literal prefix must be refused, but it applied \
                 and the accepted document reads {text:?} (doubled numbering)"
            );
        }
        Err(e) => e,
    };
    assert_eq!(
        format!("{:?}", err.code),
        "PrefixDuplicatesLabel",
        "refusal must carry the dedicated code, got {:?}: {}",
        err.code,
        err.message
    );
    // The message must name the duplicated label and what the paragraph reads.
    assert!(
        err.message.contains("1.") && err.message.contains("Events"),
        "refusal must name the label and current text: {}",
        err.message
    );
}

/// WHOLE-PARAGRAPH PATH: a replace whose content re-includes the "1.\t" label
/// plus a real body change must be REFUSED too — NOT silently stripped (which
/// dropped the agent's label and applied the rest with applied=true). Same
/// contract as the span path.
#[test]
fn whole_paragraph_replace_duplicating_literal_prefix_is_refused() {
    let doc = Document::parse(&numbered_heading_docx()).expect("parse");
    let (block_id, _guard) = first_block_id_and_guard(&doc);
    assert_literal_prefix_present(&doc, &block_id);

    // Content echoes "1.\t" AND changes the body ("Events" -> "Events and Notices").
    let result = apply_steps(
        &doc,
        vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "Events".to_string(),
            semantic_hash: None,
            content: text_content("1.\tEvents and Notices"),
        }],
    );

    let err = match result {
        Ok(_) => panic!(
            "whole-paragraph replace duplicating the literal prefix must be refused, not \
             silently stripped"
        ),
        Err(e) => e,
    };
    assert_eq!(
        format!("{:?}", err.code),
        "PrefixDuplicatesLabel",
        "refusal must carry the dedicated code, got {:?}: {}",
        err.code,
        err.message
    );
}

// ─── No overreach: the legitimate cases stay green ───────────────────────────

/// A paragraph WITHOUT any numbering (no literal_prefix, no numPr) keeps a
/// literal "1.\t" the agent writes as REAL content — there is nothing to
/// duplicate, so the guard must not fire.
#[test]
fn literal_number_on_unnumbered_paragraph_is_a_real_edit() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>Events</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let (block_id, _guard) = first_block_id_and_guard(&doc);
    // Precondition: no literal_prefix on this plain paragraph.
    assert!(
        find_paragraph(&doc, &block_id).literal_prefix.is_none(),
        "fixture: plain paragraph has no literal_prefix"
    );

    let applied = apply_steps(
        &doc,
        vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "Events".to_string(),
            semantic_hash: None,
            content: text_content("1.\tEvents"),
        }],
    )
    .expect("a literal '1.' on an unnumbered paragraph is real content, not a duplication");

    let text = accepted_full_text(&applied, &block_id);
    assert!(
        text.contains("1.") && text.contains("Events"),
        "the literal '1.\\tEvents' must be applied as real content, got {text:?}"
    );
}

/// Refuse-on-duplicate must NOT make legitimate (re)lettering impossible.
/// Adding a letter like "(b)" to an UNLETTERED paragraph
/// — one with NO existing label — is real content. The guard only fires when the paragraph
/// ALREADY carries a label, so writing "(b)\t…" onto an unlabeled paragraph is
/// real content and applies. (Re-lettering an ALREADY-lettered paragraph is the
/// corruption case, refused above; the agent uses the renumber verb for that.)
#[test]
fn adding_a_letter_to_an_unlettered_paragraph_is_a_real_edit() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>The Company is duly organized.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let (block_id, _guard) = first_block_id_and_guard(&doc);
    assert!(
        find_paragraph(&doc, &block_id).literal_prefix.is_none(),
        "fixture: the rep paragraph has no label yet"
    );

    let applied = apply_steps(
        &doc,
        vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "The Company".to_string(),
            semantic_hash: None,
            content: text_content("(b)\tThe Company is duly organized."),
        }],
    )
    .expect("adding a letter to an unlettered paragraph is real content, not a duplication");

    let text = accepted_full_text(&applied, &block_id);
    assert!(
        text.contains("(b)") && text.contains("duly organized"),
        "the new '(b)' letter must be applied as real content, got {text:?}"
    );
}

/// A DIFFERENT label at the head ("2.\t" when the paragraph's label is "1.")
/// also CORRUPTS, not renumbers. VERIFIED against the pre-guard behavior: the
/// paragraph's hoisted "1." label survives and is re-emitted, so applying
/// "2.\tEvents" verbatim renders the accepted text "1.2.\tEvents" — mixed
/// doubling, the same corruption class this contract exists to kill, through a
/// different door. (Making a literal in-body label SUPERSEDE the existing one
/// would require stripping the hoisted label — that is the `set_numbering`
/// renumber verb's job, not a side effect of typing a number into the body.)
///
/// So the guard refuses ANY enumeration label at the head of the content when
/// the paragraph already carries one — but with a DIFFERENT, intent-preserving
/// message than the echo case: it names BOTH labels, shows the stacking the
/// agent didn't intend ("1.2.…"), and says label changes via text replace are
/// unsupported. Telling a renumbering agent to "omit the label (the numbering
/// is already present)" would be factually wrong and silently abandon its
/// intent.
#[test]
fn different_label_at_head_is_refused_too() {
    let doc = Document::parse(&numbered_heading_docx()).expect("parse");
    let (block_id, _guard) = first_block_id_and_guard(&doc);
    assert_literal_prefix_present(&doc, &block_id);

    let result = apply_steps(
        &doc,
        vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "Events".to_string(),
            semantic_hash: None,
            // A different label — applying it verbatim would read "1.2.\tEvents".
            content: text_content("2.\tEvents"),
        }],
    );
    let err = match result {
        Ok(applied) => {
            let text = accepted_full_text(&applied, &block_id);
            panic!(
                "a different head label on a numbered paragraph must be refused, but it applied \
                 and reads {text:?} (doubled numbering)"
            );
        }
        Err(e) => e,
    };
    assert_eq!(
        format!("{:?}", err.code),
        "PrefixDuplicatesLabel",
        "refusal carries the dedicated code, got {:?}: {}",
        err.code,
        err.message
    );
    // The message names the leading label it caught and what the paragraph reads.
    assert!(
        err.message.contains("2.") && err.message.contains("1.\tEvents"),
        "message must name the offending label and the current text: {}",
        err.message
    );
    // And it must be truthful for the renumber case: '2.' does NOT "duplicate"
    // the label — it stacks onto it. The message says label changes via text
    // replace are unsupported, preserving the agent's renumber intent instead
    // of advising it away.
    assert!(
        err.message.contains("not supported"),
        "renumber refusal must say label changes via text replace are unsupported: {}",
        err.message
    );
    assert!(
        !err.message.contains("which duplicates"),
        "renumber refusal must not claim a different label 'duplicates' the \
         paragraph's label: {}",
        err.message
    );
}

/// A span splice on a paragraph WITHOUT numbering that inserts text starting
/// with a label is fine (no literal_prefix to duplicate).
#[test]
fn span_splice_with_label_on_unnumbered_paragraph_is_allowed() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t>Events</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let events_handle = handle_of_span(&doc, "Events");

    let result = apply_steps(
        &doc,
        vec![EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(events_handle),
            content: text_content("1.\tEvents"),
            rationale: None,
        }],
    );
    assert!(
        result.is_ok(),
        "a label on an unnumbered paragraph is real content: {:?}",
        result.err().map(|e| e.message)
    );
}
