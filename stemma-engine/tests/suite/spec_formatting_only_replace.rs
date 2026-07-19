//! A FORMATTING-ONLY replace (same text, a run's marks change — e.g. bolding one
//! word in suggesting mode) must produce a SURGICAL per-run tracked rPrChange, NOT
//! a whole-paragraph delete+insert. This is the representation `set_format` already
//! produces for font/color; bold/italic/underline (which commit via a styled
//! ReplaceParagraphText) now produce it too. Both ADD (bolding) and REMOVE
//! (un-bolding) work.
//!
//! The inherit boundary: a SAME-TEXT replace is authoritative about marks (so plain
//! content over bold un-formats); a genuine text EDIT still INHERITS the run
//! formatting (the LLM contract — see edit_basic::unchanged_text_preserves_marks).
//!
//! Daily tier, corpus-free.

use stemma::Resolution;
use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, Mark, NodeId, RevisionInfo, TrackingStatus};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, InlineMarkSet, MaterializationMode,
    ParagraphContent,
};

fn make_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
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

fn first_block_id(canon: &CanonDoc) -> NodeId {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("not a paragraph"),
    }
}

/// (status, text, is_bold, has_formatting_change, prev_marks_had_bold) per run.
fn fmt_runs(canon: &CanonDoc) -> Vec<(&'static str, String, bool, bool, bool)> {
    let mut out = Vec::new();
    if let BlockNode::Paragraph(p) = &canon.blocks[0].block {
        for seg in &p.segments {
            let tag = match seg.status {
                TrackingStatus::Normal => "normal",
                TrackingStatus::Inserted(_) => "ins",
                TrackingStatus::Deleted(_) => "del",
                TrackingStatus::InsertedThenDeleted(_) => "insdel",
            };
            for inline in &seg.inlines {
                if let InlineNode::Text(t) = inline {
                    let has_fc = t.formatting_change.is_some();
                    let prev_bold = t
                        .formatting_change
                        .as_ref()
                        .is_some_and(|fc| fc.previous_marks.contains(&Mark::Bold));
                    out.push((
                        tag,
                        t.text.clone(),
                        t.marks.contains(&Mark::Bold),
                        has_fc,
                        prev_bold,
                    ));
                }
            }
        }
    }
    out
}

fn para_text(canon: &CanonDoc) -> String {
    fmt_runs(canon)
        .into_iter()
        .map(|(_, t, _, _, _)| t)
        .collect()
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: Some("2026-06-30T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn bold() -> InlineMarkSet {
    InlineMarkSet {
        bold: true,
        ..InlineMarkSet::default()
    }
}

fn replace_step(id: NodeId, expect: &str, fragments: Vec<ContentFragment>) -> EditStep {
    EditStep::ReplaceParagraphText {
        block_id: id,
        rationale: None,
        replacement_role: None,
        expect: expect.to_string(),
        semantic_hash: None,
        content: ParagraphContent { fragments },
    }
}

const TEXT: &str = "This is a test now foo bar baz";

/// Bold one word → exactly that run gets an rPrChange; no del/ins; accept keeps the
/// bold, reject restores the pristine original.
#[test]
fn bolding_one_word_emits_rprchange_not_delins() {
    let doc = Document::parse(&make_docx(TEXT)).unwrap();
    let id = first_block_id(&doc.snapshot().canonical);

    let edited = doc
        .apply(&txn(
            vec![replace_step(
                id,
                TEXT,
                vec![
                    ContentFragment::Text("This is a test ".to_string()),
                    ContentFragment::StyledText {
                        text: "now".to_string(),
                        marks: bold(),
                    },
                    ContentFragment::Text(" foo bar baz".to_string()),
                ],
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("formatting-only replace applies");

    let r = fmt_runs(&edited.snapshot().canonical);
    // SURGICAL: no delete/insert segments — the text is unchanged.
    assert!(
        r.iter().all(|(s, _, _, _, _)| *s == "normal"),
        "every run stays Normal (no del/ins), got {r:?}"
    );
    // Exactly the "now" run carries an rPrChange; it is bold; its previous_marks had
    // no bold (an ADD).
    let fc: Vec<_> = r.iter().filter(|(_, _, _, has, _)| *has).collect();
    assert_eq!(
        fc.len(),
        1,
        "exactly one run carries an rPrChange, got {r:?}"
    );
    assert_eq!(fc[0].1, "now");
    assert!(fc[0].2, "the 'now' run is bold, got {r:?}");
    assert!(!fc[0].4, "bold ADD → previous_marks has no bold, got {r:?}");
    // Surrounding runs: unchanged, no rPrChange.
    assert!(
        r.iter()
            .any(|(s, t, b, has, _)| *s == "normal" && t == "This is a test " && !*b && !*has),
        "leading text untouched, got {r:?}"
    );
    assert!(
        r.iter()
            .any(|(s, t, b, has, _)| *s == "normal" && t == " foo bar baz" && !*b && !*has),
        "trailing text untouched, got {r:?}"
    );

    // accept-all → "now" bold, no rPrChange survives.
    let acc = edited.project(Resolution::AcceptAll).expect("accept-all");
    assert_eq!(para_text(&acc.snapshot().canonical), TEXT);
    let ar = fmt_runs(&acc.snapshot().canonical);
    assert!(
        ar.iter().any(|(_, t, b, _, _)| t == "now" && *b),
        "accept: 'now' bold, got {ar:?}"
    );
    assert!(
        !ar.iter().any(|(_, _, _, has, _)| *has),
        "accept: no rPrChange survives, got {ar:?}"
    );

    // reject-all → pristine original (nothing bold), no rPrChange.
    let rej = edited.project(Resolution::RejectAll).expect("reject-all");
    assert_eq!(para_text(&rej.snapshot().canonical), TEXT);
    let rr = fmt_runs(&rej.snapshot().canonical);
    assert!(
        !rr.iter().any(|(_, _, b, _, _)| *b),
        "reject: nothing bold, got {rr:?}"
    );
    assert!(
        !rr.iter().any(|(_, _, _, has, _)| *has),
        "reject: no rPrChange survives, got {rr:?}"
    );
}

/// Un-bolding the ONLY bold run sends all-plain content (no styled fragment) — a
/// same-text replace that REMOVES the mark (the un-format direction).
#[test]
fn unbolding_one_word_emits_rprchange() {
    // Build a clean "now"-bold state: bold it, then accept-all.
    let doc0 = Document::parse(&make_docx(TEXT)).unwrap();
    let id0 = first_block_id(&doc0.snapshot().canonical);
    let bolded = doc0
        .apply(&txn(
            vec![replace_step(
                id0,
                TEXT,
                vec![
                    ContentFragment::Text("This is a test ".to_string()),
                    ContentFragment::StyledText {
                        text: "now".to_string(),
                        marks: bold(),
                    },
                    ContentFragment::Text(" foo bar baz".to_string()),
                ],
            )],
            MaterializationMode::TrackedChange,
        ))
        .unwrap()
        .project(Resolution::AcceptAll)
        .unwrap();
    assert!(
        fmt_runs(&bolded.snapshot().canonical)
            .iter()
            .any(|(_, t, b, _, _)| t == "now" && *b),
        "precondition: 'now' is bold"
    );

    // Un-bold "now": all-plain content, same text (the realistic shape — no run
    // carries a mark anymore).
    let id = first_block_id(&bolded.snapshot().canonical);
    let unbolded = bolded
        .apply(&txn(
            vec![replace_step(
                id,
                TEXT,
                vec![ContentFragment::Text(TEXT.to_string())],
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("un-bold replace applies as a tracked change");

    let r = fmt_runs(&unbolded.snapshot().canonical);
    assert!(
        r.iter().all(|(s, _, _, _, _)| *s == "normal"),
        "no del/ins, got {r:?}"
    );
    let fc: Vec<_> = r.iter().filter(|(_, _, _, has, _)| *has).collect();
    assert_eq!(fc.len(), 1, "exactly one rPrChange, got {r:?}");
    assert_eq!(fc[0].1, "now");
    assert!(!fc[0].2, "the 'now' run is no longer bold, got {r:?}");
    assert!(fc[0].4, "bold REMOVE → previous_marks HAD bold, got {r:?}");

    // accept → not bold; reject → bold restored.
    let acc = unbolded.project(Resolution::AcceptAll).unwrap();
    assert!(
        !fmt_runs(&acc.snapshot().canonical)
            .iter()
            .any(|(_, t, b, _, _)| t == "now" && *b),
        "accept un-bold: 'now' not bold"
    );
    let rej = unbolded.project(Resolution::RejectAll).unwrap();
    assert!(
        fmt_runs(&rej.snapshot().canonical)
            .iter()
            .any(|(_, t, b, _, _)| t == "now" && *b),
        "reject un-bold: 'now' bold restored"
    );
}

/// A genuine text EDIT over bold INHERITS the bold (the LLM contract) — the
/// un-format path must NOT fire when the text changes.
#[test]
fn text_edit_over_bold_still_inherits() {
    // Build "now"-bold, then a DIFFERENT-text all-plain replace (append " here").
    let doc0 = Document::parse(&make_docx("now")).unwrap();
    let id0 = first_block_id(&doc0.snapshot().canonical);
    let bolded = doc0
        .apply(&txn(
            vec![replace_step(
                id0,
                "now",
                vec![ContentFragment::StyledText {
                    text: "now".to_string(),
                    marks: bold(),
                }],
            )],
            MaterializationMode::TrackedChange,
        ))
        .unwrap()
        .project(Resolution::AcceptAll)
        .unwrap();
    let id = first_block_id(&bolded.snapshot().canonical);
    let edited = bolded
        .apply(&txn(
            vec![replace_step(
                id,
                "now",
                vec![ContentFragment::Text("now here".to_string())],
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("text edit applies");
    // The kept "now" stays bold (inherit) and carries NO rPrChange.
    let r = fmt_runs(
        &edited
            .project(Resolution::AcceptAll)
            .unwrap()
            .snapshot()
            .canonical,
    );
    assert!(
        r.iter().any(|(_, t, b, _, _)| t.contains("now") && *b),
        "kept 'now' inherits bold (not un-formatted), got {r:?}"
    );
}

/// An unrelated text edit elsewhere in the paragraph must NOT silently accept a
/// pending tracked formatting change — the rPrChange on "now" must survive.
#[test]
fn rprchange_survives_an_unrelated_edit_in_same_paragraph() {
    let doc0 = Document::parse(&make_docx(TEXT)).unwrap();
    let id = first_block_id(&doc0.snapshot().canonical);
    let doc1 = doc0
        .apply(&txn(
            vec![replace_step(
                id,
                TEXT,
                vec![
                    ContentFragment::Text("This is a test ".to_string()),
                    ContentFragment::StyledText {
                        text: "now".to_string(),
                        marks: bold(),
                    },
                    ContentFragment::Text(" foo bar baz".to_string()),
                ],
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("bold 'now'");
    assert!(
        fmt_runs(&doc1.snapshot().canonical)
            .iter()
            .any(|(_, t, _, has, _)| t == "now" && *has),
        "precondition: 'now' carries the rPrChange"
    );

    // Unrelated edit: baz -> QUUX (tracked). "now" stays bold (sent as StyledText).
    let id1 = first_block_id(&doc1.snapshot().canonical);
    let doc2 = doc1
        .apply(&txn(
            vec![replace_step(
                id1,
                "This is a test now foo bar baz",
                vec![
                    ContentFragment::Text("This is a test ".to_string()),
                    ContentFragment::StyledText {
                        text: "now".to_string(),
                        marks: bold(),
                    },
                    ContentFragment::Text(" foo bar QUUX".to_string()),
                ],
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("unrelated edit applies");

    let r = fmt_runs(&doc2.snapshot().canonical);
    assert!(
        r.iter().any(|(_, t, _, has, _)| t == "now" && *has),
        "'now' KEEPS its rPrChange after an unrelated edit (not silently accepted), got {r:?}"
    );
}

/// Direct (untracked) mode bakes the formatting in immediately: no rPrChange, the
/// run is just bold.
#[test]
fn direct_mode_formatting_only_replace_bakes_in() {
    let doc = Document::parse(&make_docx(TEXT)).unwrap();
    let id = first_block_id(&doc.snapshot().canonical);
    let edited = doc
        .apply(&txn(
            vec![replace_step(
                id,
                TEXT,
                vec![
                    ContentFragment::Text("This is a test ".to_string()),
                    ContentFragment::StyledText {
                        text: "now".to_string(),
                        marks: bold(),
                    },
                    ContentFragment::Text(" foo bar baz".to_string()),
                ],
            )],
            MaterializationMode::Direct,
        ))
        .expect("direct formatting replace applies");
    let r = fmt_runs(&edited.snapshot().canonical);
    assert_eq!(para_text(&edited.snapshot().canonical), TEXT);
    assert!(
        r.iter().all(|(s, _, _, _, _)| *s == "normal"),
        "all Normal, got {r:?}"
    );
    assert!(
        !r.iter().any(|(_, _, _, has, _)| *has),
        "direct mode: no rPrChange, got {r:?}"
    );
    assert!(
        r.iter().any(|(_, t, b, _, _)| t == "now" && *b),
        "direct mode: 'now' baked bold, got {r:?}"
    );
}
