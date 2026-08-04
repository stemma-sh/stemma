//! Effective indent is a VIEW over (style chain, numbering, direct `w:ind`,
//! body tab presence) — ISO 29500-1 §17.3.1.12 plus the model's tab-absorption
//! contract (a style hanging indent absorbed into `left` when the body text
//! carries a tab). Projection can change those inputs: rejecting an insertion
//! can remove the very tab the absorption keyed on. The projected paragraph
//! must then carry the indent a save/reopen of the projection would resolve —
//! not the pre-projection view (wave-8 lifecycles 47/257 shape).

use std::collections::HashSet;

use stemma::api::Document;
use stemma::tracked_model::ResolveSelectionAction;
use stemma::{ExportOptions, Resolution};

/// One paragraph, style `Num` (left=879, hanging=879), whose only tab lives in
/// tracked INSERTED text: `[ins "1.\t"]["In this Schedule —"]`.
fn docx_with_inserted_tab_label() -> Vec<u8> {
    let body = concat!(
        r#"<w:p><w:pPr><w:pStyle w:val="Num"/></w:pPr>"#,
        r#"<w:ins w:id="11" w:author="Reviewer" w:date="2026-06-01T00:00:00Z">"#,
        r#"<w:r><w:t xml:space="preserve">1.</w:t></w:r><w:r><w:tab/></w:r></w:ins>"#,
        r#"<w:r><w:t xml:space="preserve">In this Schedule —</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let styles = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:styleId="Num"><w:name w:val="Num"/>"#,
        r#"<w:pPr><w:ind w:left="879" w:hanging="879"/></w:pPr></w:style>"#,
        r#"</w:styles>"#,
    );
    make_docx(body, &[("word/styles.xml", styles)])
}

fn body_indent(doc: &Document) -> (Option<i32>, Option<i32>) {
    let canon = doc.snapshot().canonical.clone();
    let stemma::BlockNode::Paragraph(p) = &canon.blocks[0].block else {
        panic!("first block is a paragraph");
    };
    let indent = p.indent.as_ref().expect("paragraph carries an indent view");
    (indent.left, indent.effective_first_line_twips)
}

/// Rejecting the insertion removes the paragraph's only tab, so the hanging
/// indent the import absorbed into `left` (879 − 879 = 0) must un-absorb: the
/// projection reports the same (879, −879) a save/reopen of it resolves.
#[test]
fn rejecting_the_tab_bearing_insertion_unabsorbs_the_style_hanging_indent() {
    let doc = Document::parse(&docx_with_inserted_tab_label()).expect("parse");
    assert_eq!(
        body_indent(&doc),
        (Some(0), None),
        "imported: the tab in the inserted label absorbs the style hanging indent"
    );

    let rejected = doc.project(Resolution::RejectAll).expect("reject all");
    let projected = body_indent(&rejected);

    let saved = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize projection");
    let reopened = Document::parse(&saved).expect("reopen projection");
    assert_eq!(
        projected,
        body_indent(&reopened),
        "the in-memory projection and its save/reopen agree on the indent view"
    );
    assert_eq!(
        projected,
        (Some(879), Some(-879)),
        "with the tab gone, the style chain's left=879/hanging=879 governs"
    );
}

/// One paragraph, style `Sp` (line=276/auto), carrying a pPrChange whose
/// previous direct pPr is `after=320` under the same style.
fn docx_with_spacing_ppr_change() -> Vec<u8> {
    let body = concat!(
        r#"<w:p><w:pPr><w:pStyle w:val="Sp"/><w:spacing w:after="100"/>"#,
        r#"<w:pPrChange w:id="9" w:author="Reviewer" w:date="2026-06-01T00:00:00Z">"#,
        r#"<w:pPr><w:pStyle w:val="Sp"/><w:spacing w:after="320"/></w:pPr></w:pPrChange>"#,
        r#"</w:pPr><w:r><w:t xml:space="preserve">Spacing body.</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let styles = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:styleId="Sp"><w:name w:val="Sp"/>"#,
        r#"<w:pPr><w:spacing w:line="276" w:lineRule="auto"/></w:pPr></w:style>"#,
        r#"</w:styles>"#,
    );
    make_docx(body, &[("word/styles.xml", styles)])
}

fn body_spacing(doc: &Document) -> (Option<u32>, Option<u32>) {
    let canon = doc.snapshot().canonical.clone();
    let stemma::BlockNode::Paragraph(p) = &canon.blocks[0].block else {
        panic!("first block is a paragraph");
    };
    let spacing = p
        .spacing
        .as_ref()
        .expect("paragraph carries a spacing view");
    (spacing.after, spacing.line)
}

/// §17.3.1.33 is a per-attribute cascade: rejecting the spacing change
/// restores the previous DIRECT `after`, and every attribute the direct pPr
/// omits — here the style's `line` — still inherits. The restored snapshot
/// alone is not the effective state (wave-8 lifecycle 704).
#[test]
fn rejecting_a_spacing_change_keeps_the_style_chains_line_spacing() {
    let doc = Document::parse(&docx_with_spacing_ppr_change()).expect("parse");
    assert_eq!(
        body_spacing(&doc),
        (Some(100), Some(276)),
        "imported: live direct after=100 merged with the style line=276"
    );

    let rejected = doc.project(Resolution::RejectAll).expect("reject all");
    let projected = body_spacing(&rejected);

    let saved = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize projection");
    let reopened = Document::parse(&saved).expect("reopen projection");
    assert_eq!(
        projected,
        body_spacing(&reopened),
        "the in-memory projection and its save/reopen agree on the spacing view"
    );
    assert_eq!(
        projected,
        (Some(320), Some(276)),
        "restored direct after=320, style line=276 still inherited"
    );
}

/// A pPrChange whose paragraph-mark formatting did not change must read back
/// as exactly that. The serializer persists "mark unchanged" by omitting the
/// inner pPr's rPr; importing that omission as "the previous mark had no
/// formatting" gave the SAME revision two snapshots — full in the authoring
/// session, empty after a save/reopen (wave-8 session-split class).
#[test]
fn an_unchanged_paragraph_mark_snapshot_survives_save_and_reopen() {
    let body = concat!(
        r#"<w:p><w:pPr><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial"/>"#,
        r#"<w:sz w:val="22"/></w:rPr></w:pPr>"#,
        r#"<w:r><w:t xml:space="preserve">Target text here.</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let styles = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
        r#"</w:styles>"#,
    );
    let doc = Document::parse(&make_docx(body, &[("word/styles.xml", styles)])).expect("parse");
    let block_id = {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(p) = &canon.blocks[0].block else {
            panic!("paragraph");
        };
        p.id.clone()
    };
    let edited = doc
        .apply(&stemma::edit::EditTransaction {
            steps: vec![stemma::edit::EditStep::SetParagraphFormatting {
                block_id,
                semantic_hash: None,
                patch: stemma::edit::ParagraphFormattingPatch {
                    align: Some(stemma::Alignment::Center),
                    indent: None,
                    spacing: None,
                    borders: None,
                    shading: None,
                },
                rationale: None,
            }],
            summary: None,
            materialization_mode: stemma::edit::MaterializationMode::TrackedChange,
            revision: stemma::domain::RevisionInfo {
                revision_id: 5,
                identity: 0,
                author: Some("Reviewer".into()),
                date: Some("2026-06-01T00:00:00Z".into()),
                apply_op_id: None,
            },
        })
        .expect("apply align change");

    // The FULL props, `preserved` included: the historical drift was not in
    // the typed fields but in the source-form provenance list, which is
    // canonical-tree state like everything else.
    let snapshot_mark = |doc: &Document| {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(p) = &canon.blocks[0].block else {
            panic!("paragraph");
        };
        let fc = p.formatting_change.as_ref().expect("pPrChange present");
        fc.previous_paragraph_mark_style_props.clone()
    };
    let authored = snapshot_mark(&edited);
    assert_eq!(
        (authored.font_family.clone(), authored.font_size),
        (Some("Arial".into()), Some(22)),
        "the snapshot records the mark formatting that was in effect"
    );

    let saved = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reopened = Document::parse(&saved).expect("reopen");
    assert_eq!(
        snapshot_mark(&reopened),
        authored,
        "an align-only pPrChange leaves the mark snapshot exactly as authored          across a save/reopen — absent inner rPr means unchanged, not empty"
    );
}

/// An inserted paragraph's id is unique against the WHOLE tree, nested cell
/// blocks included.
///
/// The role machinery clones its exemplar paragraph — which can live inside a
/// table cell — and the id allocator used to collision-check only top-level
/// blocks, so the insert kept the cell exemplar's id verbatim. Two paragraphs
/// then answered to one name, and a later step addressing the new paragraph
/// formatted the CELL paragraph the caller never named (wave-8 lifecycle 385).
#[test]
fn an_inserted_paragraphs_id_is_unique_against_cell_paragraphs() {
    let body = concat!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tr><w:tc>"#,
        r#"<w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr>"#,
        r#"<w:p><w:r><w:t>Cell body one.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Cell body two.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Cell body three.</w:t></w:r></w:p>"#,
        r#"</w:tc></w:tr></w:tbl>"#,
        r#"<w:p><w:pPr><w:pStyle w:val="Anchor"/></w:pPr><w:r><w:t>Anchor paragraph.</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let styles = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="Anchor"><w:name w:val="Anchor"/>"#,
        r#"<w:pPr><w:ind w:left="1440"/></w:pPr></w:style>"#,
        r#"</w:styles>"#,
    );
    let doc = Document::parse(&make_docx(body, &[("word/styles.xml", styles)])).expect("parse");
    let anchor_id = {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(p) = &canon.blocks[1].block else {
            panic!("anchor paragraph");
        };
        p.id.clone()
    };
    let edited = doc
        .apply(&stemma::edit::EditTransaction {
            steps: vec![stemma::edit::EditStep::InsertParagraphs {
                anchor_block_id: anchor_id,
                position: stemma::edit::InsertPosition::After,
                rationale: None,
                blocks: vec![stemma::edit::BlockSpec::Paragraph(
                    stemma::edit::ParagraphBlockSpec {
                        role: Some("default".to_string()),
                        content: stemma::edit::ParagraphContent {
                            fragments: vec![stemma::edit::ContentFragment::Text(
                                "Inserted body paragraph.".to_string(),
                            )],
                        },
                        restart_numbering: false,
                        list: None,
                    },
                )],
            }],
            summary: None,
            materialization_mode: stemma::edit::MaterializationMode::TrackedChange,
            revision: stemma::domain::RevisionInfo {
                revision_id: 9,
                identity: 0,
                author: Some("Reviewer".into()),
                date: Some("2026-06-01T00:00:00Z".into()),
                apply_op_id: None,
            },
        })
        .expect("insert with the cell-majority default role");

    let canon = edited.snapshot().canonical.clone();
    let mut ids: Vec<String> = Vec::new();
    fn collect(block: &stemma::BlockNode, ids: &mut Vec<String>) {
        match block {
            stemma::BlockNode::Paragraph(p) => ids.push(p.id.0.to_string()),
            stemma::BlockNode::Table(t) => {
                ids.push(t.id.0.to_string());
                for row in &t.rows {
                    for cell in &row.cells {
                        for nested in &cell.blocks {
                            collect(nested, ids);
                        }
                    }
                }
            }
            stemma::BlockNode::OpaqueBlock(o) => ids.push(o.id.0.to_string()),
        }
    }
    for tracked in &canon.blocks {
        collect(&tracked.block, &mut ids);
    }
    let mut seen = std::collections::HashSet::new();
    let duplicates: Vec<&String> = ids
        .iter()
        .filter(|id| !seen.insert((*id).clone()))
        .collect();
    assert!(
        duplicates.is_empty(),
        "every block in the tree answers to a unique name; duplicated: {duplicates:?}"
    );
}

/// A role-less insert inherits from the HOST paragraph, Word-style.
///
/// Word's insertion range for insert-after sits past the anchor's pilcrow —
/// inside the FOLLOWING paragraph — and typed content inherits that
/// paragraph's pPr and leading run formatting (adjudicated on the live
/// oracle: an insert between two hanging-indent list items carried their
/// indent; an insert before a bold heading came back bold). The engine's
/// `role: None` insert models exactly that, in both materializations.
#[test]
fn a_role_less_insert_inherits_the_following_paragraphs_formatting() {
    let body = concat!(
        r#"<w:p><w:r><w:t>Plain anchor paragraph.</w:t></w:r></w:p>"#,
        r#"<w:p><w:pPr><w:ind w:left="1440" w:hanging="720"/></w:pPr>"#,
        r#"<w:r><w:rPr><w:b/></w:rPr><w:t>Bold hanging follower.</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let styles = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
        r#"</w:styles>"#,
    );
    let doc = Document::parse(&make_docx(body, &[("word/styles.xml", styles)])).expect("parse");
    let anchor_id = {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(p) = &canon.blocks[0].block else {
            panic!("anchor");
        };
        p.id.clone()
    };
    let step = stemma::edit::EditStep::InsertParagraphs {
        anchor_block_id: anchor_id,
        position: stemma::edit::InsertPosition::After,
        rationale: None,
        blocks: vec![stemma::edit::BlockSpec::Paragraph(
            stemma::edit::ParagraphBlockSpec {
                role: None,
                content: stemma::edit::ParagraphContent {
                    fragments: vec![stemma::edit::ContentFragment::Text(
                        "Inserted between.".to_string(),
                    )],
                },
                restart_numbering: false,
                list: None,
            },
        )],
    };
    let inserted_state = |doc: &Document| {
        let canon = doc.snapshot().canonical.clone();
        for tb in &canon.blocks {
            let stemma::BlockNode::Paragraph(p) = &tb.block else {
                continue;
            };
            let text: String = p
                .segments
                .iter()
                .flat_map(|s| &s.inlines)
                .filter_map(|i| match i {
                    stemma::domain::InlineNode::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect();
            if text.contains("Inserted between") {
                let marks = p
                    .segments
                    .iter()
                    .flat_map(|s| &s.inlines)
                    .find_map(|i| match i {
                        stemma::domain::InlineNode::Text(t) => Some(t.marks.clone()),
                        _ => None,
                    })
                    .expect("inserted run");
                return (
                    p.indent
                        .as_ref()
                        .map(|i| (i.left, i.effective_first_line_twips)),
                    marks,
                );
            }
        }
        panic!("inserted paragraph not found");
    };
    for mode in [
        stemma::edit::MaterializationMode::TrackedChange,
        stemma::edit::MaterializationMode::Direct,
    ] {
        let edited = doc
            .apply(&stemma::edit::EditTransaction {
                steps: vec![step.clone()],
                summary: None,
                materialization_mode: mode,
                revision: stemma::domain::RevisionInfo {
                    revision_id: 9,
                    identity: 0,
                    author: Some("Reviewer".into()),
                    date: Some("2026-06-01T00:00:00Z".into()),
                    apply_op_id: None,
                },
            })
            .expect("role-less insert applies");
        let (indent, marks) = inserted_state(&edited);
        assert_eq!(
            indent,
            Some((Some(1440), Some(-720))),
            "{mode:?}: the inserted paragraph carries the follower's hanging indent"
        );
        assert_eq!(
            marks,
            vec![stemma::domain::Mark::Bold],
            "{mode:?}: and its leading run formatting"
        );
        let saved = edited
            .serialize(&ExportOptions::default())
            .expect("serialize");
        let reopened = Document::parse(&saved).expect("reopen");
        let (r_indent, r_marks) = inserted_state(&reopened);
        assert_eq!(
            (r_indent, r_marks),
            (indent, marks),
            "{mode:?}: stable across save/reopen"
        );
    }
}

/// A literal prefix is hoisted out of the paragraph's ordinary inline stream,
/// but it remains the first physical run at Word's insert-before boundary.
/// The new paragraph inherits that run's formatting, not the first body run's
/// potentially different formatting.
#[test]
fn a_role_less_insert_before_inherits_the_literal_prefix_runs_formatting() {
    let body = concat!(
        r#"<w:p>"#,
        r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">D. </w:t></w:r>"#,
        r#"<w:r><w:rPr><w:b/><w:u w:val="single"/></w:rPr><w:t>Underlined body.</w:t></w:r>"#,
        r#"</w:p><w:sectPr/>"#,
    );
    let doc = Document::parse(&make_docx(body, &[])).expect("parse");
    let anchor_id = {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(paragraph) = &canon.blocks[0].block else {
            panic!("anchor");
        };
        assert_eq!(paragraph.literal_prefix.as_deref(), Some("D."));
        paragraph.id.clone()
    };
    let step = stemma::edit::EditStep::InsertParagraphs {
        anchor_block_id: anchor_id,
        position: stemma::edit::InsertPosition::Before,
        rationale: None,
        blocks: vec![stemma::edit::BlockSpec::Paragraph(
            stemma::edit::ParagraphBlockSpec {
                role: None,
                content: stemma::edit::ParagraphContent {
                    fragments: vec![stemma::edit::ContentFragment::Text(
                        "Inserted before prefix.".to_string(),
                    )],
                },
                restart_numbering: false,
                list: None,
            },
        )],
    };

    for mode in [
        stemma::edit::MaterializationMode::TrackedChange,
        stemma::edit::MaterializationMode::Direct,
    ] {
        let edited = doc
            .apply(&stemma::edit::EditTransaction {
                steps: vec![step.clone()],
                summary: None,
                materialization_mode: mode,
                revision: stemma::domain::RevisionInfo {
                    revision_id: 10,
                    identity: 0,
                    author: Some("Reviewer".into()),
                    date: Some("2026-08-03T00:00:00Z".into()),
                    apply_op_id: None,
                },
            })
            .expect("role-less insert applies");
        let inserted = edited
            .snapshot()
            .canonical
            .blocks
            .iter()
            .find_map(|tracked| match &tracked.block {
                stemma::BlockNode::Paragraph(paragraph)
                    if paragraph
                        .all_inlines()
                        .any(|inline| matches!(inline, stemma::domain::InlineNode::Text(text) if text.text == "Inserted before prefix.")) =>
                {
                    paragraph.first_content_text_node()
                }
                _ => None,
            })
            .expect("inserted paragraph");
        assert_eq!(
            inserted.marks,
            vec![stemma::domain::Mark::Bold],
            "{mode:?}: Word inherits the first physical prefix run, not the underlined body run"
        );
    }
}

/// A leading break is a real physical run with its own typing format. Word's
/// insertion point precedes that run; skipping to the following text can pick
/// a different format (plain break followed by bold text in this witness).
#[test]
fn a_role_less_insert_inherits_a_leading_hard_breaks_run_formatting() {
    let body = concat!(
        r#"<w:p><w:r><w:t>Anchor paragraph.</w:t></w:r></w:p>"#,
        r#"<w:p>"#,
        r#"<w:r><w:rPr><w:rFonts w:ascii="Times New Roman"/><w:sz w:val="32"/></w:rPr><w:br/></w:r>"#,
        r#"<w:r><w:rPr><w:b/><w:highlight w:val="yellow"/></w:rPr><w:t>Bold following text.</w:t></w:r>"#,
        r#"</w:p><w:sectPr/>"#,
    );
    let doc = Document::parse(&make_docx(body, &[])).expect("parse");
    let anchor_id = {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(paragraph) = &canon.blocks[0].block else {
            panic!("anchor");
        };
        paragraph.id.clone()
    };
    let step = stemma::edit::EditStep::InsertParagraphs {
        anchor_block_id: anchor_id,
        position: stemma::edit::InsertPosition::After,
        rationale: None,
        blocks: vec![stemma::edit::BlockSpec::Paragraph(
            stemma::edit::ParagraphBlockSpec {
                role: None,
                content: stemma::edit::ParagraphContent {
                    fragments: vec![stemma::edit::ContentFragment::Text(
                        "Inserted before break.".to_string(),
                    )],
                },
                restart_numbering: false,
                list: None,
            },
        )],
    };

    for mode in [
        stemma::edit::MaterializationMode::TrackedChange,
        stemma::edit::MaterializationMode::Direct,
    ] {
        let edited = doc
            .apply(&stemma::edit::EditTransaction {
                steps: vec![step.clone()],
                summary: None,
                materialization_mode: mode,
                revision: stemma::domain::RevisionInfo {
                    revision_id: 11,
                    identity: 0,
                    author: Some("Reviewer".into()),
                    date: Some("2026-08-03T00:00:00Z".into()),
                    apply_op_id: None,
                },
            })
            .expect("role-less insert applies");
        let inserted = edited
            .snapshot()
            .canonical
            .blocks
            .iter()
            .find_map(|tracked| match &tracked.block {
                stemma::BlockNode::Paragraph(paragraph)
                    if paragraph
                        .all_inlines()
                        .any(|inline| matches!(inline, stemma::domain::InlineNode::Text(text) if text.text == "Inserted before break.")) =>
                {
                    paragraph.first_content_text_node()
                }
                _ => None,
            })
            .expect("inserted paragraph");
        assert!(
            inserted.marks.is_empty(),
            "{mode:?}: the plain break run, not the following bold text run, owns the insertion point"
        );
        assert_eq!(
            inserted.style_props.font_family.as_deref(),
            Some("Times New Roman")
        );
        assert_eq!(inserted.style_props.font_size, Some(32));
        assert_eq!(inserted.style_props.highlight, None);
    }
}

/// The pPr toggles are the same per-attribute cascade as spacing: rejecting
/// a formatting change restores the previous DIRECT pPr, and a toggle the
/// snapshot omits — here the style\'s contextualSpacing — still inherits
/// (wave-8 real-word v7 lifecycle 6: the in-memory reject endpoint lost the
/// style contribution every wire agreed on).
#[test]
fn rejecting_a_formatting_change_keeps_the_style_chains_contextual_spacing() {
    let body = concat!(
        r#"<w:p><w:pPr><w:pStyle w:val="Cx"/><w:jc w:val="center"/>"#,
        r#"<w:pPrChange w:id="9" w:author="Reviewer" w:date="2026-06-01T00:00:00Z">"#,
        r#"<w:pPr><w:pStyle w:val="Cx"/></w:pPr></w:pPrChange>"#,
        r#"</w:pPr><w:r><w:t xml:space="preserve">Toggle body.</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let styles = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:styleId="Cx"><w:name w:val="Cx"/>"#,
        r#"<w:pPr><w:contextualSpacing/></w:pPr></w:style>"#,
        r#"</w:styles>"#,
    );
    let doc = Document::parse(&make_docx(body, &[("word/styles.xml", styles)])).expect("parse");
    let contextual = |doc: &Document| {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(p) = &canon.blocks[0].block else {
            panic!("paragraph");
        };
        p.contextual_spacing
    };
    assert_eq!(contextual(&doc), Some(true), "imported: style contributes");

    let rejected = doc.project(Resolution::RejectAll).expect("reject all");
    let projected = contextual(&rejected);
    let saved = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize projection");
    let reopened = Document::parse(&saved).expect("reopen projection");
    assert_eq!(
        projected,
        contextual(&reopened),
        "in-memory projection and its save/reopen agree"
    );
    assert_eq!(
        projected,
        Some(true),
        "the style chain\'s contextualSpacing survives the reject"
    );
}

/// The inner `w:pPr` of a Word-authored pPrChange is the previous DIRECT
/// paragraph state. An absent inner `w:numPr` therefore means "fall through to
/// the restored style", not "the previous paragraph was unnumbered".
#[test]
fn rejecting_a_formatting_change_restores_style_inherited_numbering() {
    let body = concat!(
        r#"<w:p><w:pPr><w:pStyle w:val="CSILevel3"/><w:jc w:val="right"/>"#,
        r#"<w:pPrChange w:id="9" w:author="Reviewer" w:date="2026-06-01T00:00:00Z">"#,
        r#"<w:pPr><w:pStyle w:val="CSILevel3"/></w:pPr></w:pPrChange>"#,
        r#"</w:pPr><w:r><w:t>Protect finishes until completion of project.</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let styles = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:styleId="CSILevel3"><w:name w:val="CSI Level 3"/>"#,
        r#"<w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>"#,
        r#"</w:style></w:styles>"#,
    );
    let numbering = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0">"#,
        r#"<w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>"#,
        r#"</w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        r#"</w:numbering>"#,
    );
    let doc = Document::parse(&make_docx(
        body,
        &[
            ("word/styles.xml", styles),
            ("word/numbering.xml", numbering),
        ],
    ))
    .expect("parse");
    let numbering_view = |doc: &Document| {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(p) = &canon.blocks[0].block else {
            panic!("paragraph");
        };
        p.numbering.as_ref().map(|numbering| {
            (
                numbering.num_id,
                numbering.ilvl,
                numbering.synthesized_text.clone(),
            )
        })
    };
    assert_eq!(
        numbering_view(&doc),
        Some((1, 0, "1.".into())),
        "the live paragraph inherits numbering from CSILevel3"
    );
    let formatting_identity = {
        let canon = doc.snapshot().canonical.clone();
        let stemma::BlockNode::Paragraph(p) = &canon.blocks[0].block else {
            panic!("paragraph");
        };
        p.formatting_change.as_ref().expect("pPrChange").identity
    };

    for (name, resolution) in [
        ("full", Resolution::RejectAll),
        (
            "selective",
            Resolution::Selective {
                ids: HashSet::from([formatting_identity]),
                action: ResolveSelectionAction::Reject,
            },
        ),
    ] {
        let rejected = doc
            .project(resolution)
            .unwrap_or_else(|error| panic!("{name} reject: {error}"));
        let projected = numbering_view(&rejected);
        let saved = rejected
            .serialize(&ExportOptions::default())
            .unwrap_or_else(|error| panic!("serialize {name} projection: {error}"));
        let reopened = Document::parse(&saved).expect("reopen projection");
        assert_eq!(
            projected,
            numbering_view(&reopened),
            "{name}: in-memory reject and its save/reopen agree on style-inherited numbering"
        );
        assert_eq!(
            projected,
            Some((1, 0, "1.".into())),
            "{name}: absent direct numPr falls through to the restored style"
        );
    }
}

/// `w:lastRenderedPageBreak` is the pagination CACHE (§17.3.3.16 — "as of
/// the last time the document was repaginated"): every producer recomputes,
/// moves or drops it on save, real Word included. It round-trips faithfully
/// as preserved content, but its presence is render state, not document
/// state — two documents differing only in it are canonically EQUAL.
#[test]
fn a_pagination_cache_marker_does_not_make_documents_differ() {
    let styles = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
        r#"</w:styles>"#,
    );
    let with_marker = concat!(
        r#"<w:p><w:r><w:t>Page top </w:t><w:lastRenderedPageBreak/><w:t>text.</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let without_marker = concat!(
        r#"<w:p><w:r><w:t>Page top text.</w:t></w:r></w:p>"#,
        r#"<w:sectPr/>"#,
    );
    let a = Document::parse(&make_docx(with_marker, &[("word/styles.xml", styles)]))
        .expect("parse with marker");
    let b = Document::parse(&make_docx(without_marker, &[("word/styles.xml", styles)]))
        .expect("parse without marker");
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        a.snapshot().canonical.as_ref(),
        b.snapshot().canonical.as_ref(),
    );
    assert!(
        differences.is_empty(),
        "pagination cache must not participate in canonical equality: {differences:?}"
    );
}

// ─── minimal-.docx plumbing (same shape as the ffData suite) ────────────────

fn make_docx(body_xml: &str, extra_parts: &[(&str, &str)]) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_xml}</w:body></w:document>"#
    );
    let mut overrides = String::from(
        r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
    );
    for (path, _) in extra_parts {
        let content_type = match *path {
            "word/styles.xml" => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"
            }
            "word/numbering.xml" => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"
            }
            other => panic!("make_docx: no content-type mapping for extra part {other}"),
        };
        overrides.push_str(&format!(
            r#"<Override PartName="/{path}" ContentType="{content_type}"/>"#
        ));
    }
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#
    );
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut doc_rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (index, (path, _)) in extra_parts.iter().enumerate() {
        let target = path.trim_start_matches("word/");
        let rel_type = match *path {
            "word/styles.xml" => {
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
            }
            "word/numbering.xml" => {
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering"
            }
            other => panic!("make_docx: no relationship mapping for extra part {other}"),
        };
        doc_rels.push_str(&format!(
            r#"<Relationship Id="rId{}" Type="{rel_type}" Target="{target}"/>"#,
            index + 10,
        ));
    }
    doc_rels.push_str("</Relationships>");

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut write = |name: &str, content: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        };
        write("[Content_Types].xml", &content_types);
        write("_rels/.rels", root_rels);
        write("word/_rels/document.xml.rels", &doc_rels);
        write("word/document.xml", &document_xml);
        for (path, xml) in extra_parts {
            write(path, xml);
        }
        zip.finish().unwrap();
    }
    buf
}
