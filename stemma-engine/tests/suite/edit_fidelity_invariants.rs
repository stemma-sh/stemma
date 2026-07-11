//! Per-verb edit-fidelity invariants — the standing gate that makes the verb
//! fan-out safe (domain-model §11; `stemma-engine/src/edit/AGENTS.md`).
//!
//! For any valid edit, three properties must hold, and they encode the recipe's
//! "done" criterion as enforceable tests:
//!
//! 1. **Reversibility.** reject-all of the edited document reconstructs the
//!    original exactly.
//! 2. **Accept equals direct.** accept-all of a tracked edit equals applying the
//!    same edit in Direct (untracked) mode.
//! 3. **Non-shrinking opaque inventory.** no opaque anchor present before the
//!    edit is missing after it (fail-never preservation).
//!
//! These run daily and are engine-independent (they use the engine's own
//! accept/reject). The Word oracle is the nightly differential layer that breaks
//! the circularity on the dark corners; this gate catches the common breakage a
//! new verb introduces.

use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::tracked_model::ResolveSelectionAction;
use stemma::view::{BlockRole, SegmentView, TextMark, TrackStatus, build_document_view_from_canon};
use stemma::{ExportOptions, Resolution, accept_all, reject_all_with_styles};

// ─── Fixtures ──────────────────────────────────────────────────────────────

/// Minimal plain-paragraph DOCX (copied from the engine's own view/api tests).
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

/// Build a DOCX whose single paragraph hosts an inline `w:drawing` with a
/// `wp:extent` (cx/cy) and a `wp:docPr` (id/name/descr). No binary media part is
/// referenced — the IR keeps the drawing XML in `raw_xml`, which is all the
/// attribute verb touches.
fn make_drawing_docx(cx: i64, cy: i64, descr: &str) -> Vec<u8> {
    let drawing = format!(
        r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="1" name="Picture 1" descr="{descr}"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:ext cx="999" cy="888"/></a:graphicData></a:graphic></wp:inline></w:drawing>"#
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r>{drawing}</w:r></w:p><w:sectPr/></w:body></w:document>"#
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

/// Find the first `OpaqueInline` of kind `Drawing`: its hosting paragraph id,
/// its own id, and its current `raw_xml`.
fn first_drawing(canon: &CanonDoc) -> (NodeId, NodeId, Vec<u8>) {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                    {
                        return (
                            p.id.clone(),
                            o.id.clone(),
                            o.raw_xml.clone().expect("drawing must carry raw_xml"),
                        );
                    }
                }
            }
        }
    }
    panic!("no drawing opaque inline found");
}

/// Parse a plain-paragraph doc; return its canonical IR and block ids in order.
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
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn text_content(text: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    }
}

/// A run-insensitive, id-insensitive fingerprint of a document's *content*:
/// block role + style, and per span its visible text + meaningful marks +
/// tracked status (or, for opaque spans, the anchor kind). This is the right
/// notion of "the same document" for an edit-fidelity invariant: run
/// re-segmentation with identical formatting is semantically irrelevant in
/// OOXML, and block/anchor ids are allocated (not content). Built over
/// `DocumentView`, which already coalesces adjacent same-(status,marks) runs.
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
    // Zip the canonical blocks with the view (1:1, document order) so the
    // fingerprint also captures paragraph-level formatting (alignment, indent,
    // spacing) that DocumentView does not surface but a pPrChange verb changes.
    // Without this, a pure alignment flip would make base and rejected shapes
    // identical even if reject were broken.
    for (tb, b) in canon.blocks.iter().zip(view.blocks.iter()) {
        let para_fmt = match &tb.block {
            BlockNode::Paragraph(p) => format!(
                "|align={:?},indent={:?},spacing={:?}",
                p.align, p.indent, p.spacing
            ),
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

/// Every opaque-inline anchor id in the document, in any order.
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

// ─── The invariant harness ───────────────────────────────────────────────────

/// Assert the three fidelity invariants for one edit (given as steps) against a
/// base document.
fn assert_fidelity(label: &str, base: &CanonDoc, steps: Vec<EditStep>) {
    let tracked = apply_transaction(
        base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .unwrap_or_else(|e| panic!("[{label}] tracked apply failed: {e}"))
    .0;

    // 1. Reversibility: reject-all reconstructs the original content.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        shape(base),
        shape(&rejected),
        "[{label}] reject-all must reconstruct the original content"
    );

    // 2. Accept equals direct.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(base, &txn(steps, MaterializationMode::Direct))
        .unwrap_or_else(|e| panic!("[{label}] direct apply failed: {e}"))
        .0;
    assert_eq!(
        shape(&accepted),
        shape(&direct),
        "[{label}] accept-all must equal direct apply"
    );

    // 3. Non-shrinking opaque inventory.
    let before = anchor_ids(base);
    let after = anchor_ids(&tracked);
    for id in &before {
        assert!(
            after.contains(id),
            "[{label}] opaque anchor '{id}' was dropped by the edit"
        );
    }
}

// ─── Per-verb cases ──────────────────────────────────────────────────────────

#[test]
fn replace_paragraph_text_is_faithful() {
    let (base, ids) = doc_and_ids(&["Hello world", "Second paragraph"]);
    assert_fidelity(
        "replace",
        &base,
        vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(ids[0].as_str()),
            rationale: None,
            replacement_role: None,
            expect: "Hello world".to_string(),
            semantic_hash: None,
            content: text_content("Goodbye world"),
        }],
    );
}

#[test]
fn replace_span_text_is_faithful() {
    // Phase 3 sub-block addressing: a whole-span ReplaceSpanText must satisfy the
    // same three fidelity invariants (reversibility, accept==direct, non-shrinking
    // opaque inventory) as any other verb — it routes through the one materializer.
    let (base, ids) = doc_and_ids(&["Hello world", "Second paragraph"]);
    assert_fidelity(
        "replace_span_whole",
        &base,
        vec![EditStep::ReplaceSpanText {
            block_id: NodeId::from(ids[0].as_str()),
            guard: stemma::semantic_hash::block_semantic_hash_for_block(&base.blocks[0].block),
            expect: None,
            span: ResolvedSpanSelector::Whole,
            content: text_content("Goodbye world"),
            rationale: None,
        }],
    );
}

#[test]
fn set_run_formatting_is_faithful() {
    let (base, ids) = doc_and_ids(&["The Confidential Information is protected."]);
    assert_fidelity(
        "run_formatting",
        &base,
        vec![EditStep::SetRunFormatting {
            block_id: NodeId::from(ids[0].as_str()),
            expect: "Confidential".to_string(),
            semantic_hash: None,
            marks: InlineMarkSet {
                bold: true,
                ..Default::default()
            },
            style: RunStyleEdit::default(),
            rationale: None,
        }],
    );
}

/// run-formatting-extended: value-bearing run-style edit (color + highlight +
/// font family + font size) as a tracked `w:rPrChange`.
///
/// The standing `shape()` fingerprint is BLIND to color/highlight/font (the
/// public view's `TextMark` only covers bold/italic/underline/strike/sub/super,
/// so a broken reversal of these properties would still pass the generic gate).
/// We therefore (a) run the generic harness — it proves the structural run-split
/// reverses and the opaque inventory is non-shrinking — and (b) assert
/// reversibility/accept==direct DIRECTLY on the IR `StyleProps`, which is the
/// only place these values are observable.
#[test]
fn set_run_formatting_extended_is_faithful() {
    let (base, ids) = doc_and_ids(&["The Confidential Information is protected."]);
    let block_id = NodeId::from(ids[0].as_str());
    let steps = vec![EditStep::SetRunFormatting {
        block_id: block_id.clone(),
        expect: "Confidential".to_string(),
        semantic_hash: None,
        marks: InlineMarkSet {
            bold: true,
            // caps / smallCaps are StyleProps tri-states, blind to shape()'s
            // TextMark fingerprint — asserted directly on the IR below.
            caps: true,
            small_caps: true,
            ..Default::default()
        },
        style: RunStyleEdit {
            color: Some("FF0000".into()),
            highlight: Some(HighlightColor::Yellow),
            font_family: Some("Arial".into()),
            font_size_half_points: Some(28),
            // char spacing (w:spacing @w:val twips) — also blind to shape().
            char_spacing: Some(40),
        },
        rationale: None,
    }];

    // (a) Structural gate (boolean marks, run-split, opaque inventory).
    assert_fidelity("run_formatting_extended", &base, steps.clone());

    // (b) Value-bearing IR assertions. The StyleProps of the run that carries
    // the matched span. In `base` the span lives inside a larger run; after the
    // edit it is its own split TextNode — substring containment finds both.
    fn styled_props(canon: &CanonDoc, span: &str) -> StyleProps {
        for tb in &canon.blocks {
            if let BlockNode::Paragraph(p) = &tb.block {
                for seg in &p.segments {
                    for inline in &seg.inlines {
                        if let InlineNode::Text(t) = inline
                            && t.text.contains(span)
                        {
                            return t.style_props.clone();
                        }
                    }
                }
            }
        }
        panic!("styled span '{span}' not found in document");
    }

    let base_props = styled_props(&base, "Confidential");

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;

    // Tracked edit set the new value-bearing props on the styled run.
    let live = styled_props(&tracked, "Confidential");
    assert_eq!(live.color.as_deref(), Some("FF0000"));
    assert_eq!(live.highlight, Some(HighlightColor::Yellow));
    assert_eq!(live.font_family.as_deref(), Some("Arial"));
    assert_eq!(live.font_size, Some(28));
    // caps / smallCaps resolve to StyleProps tri-states (On), and char spacing
    // to the twip value — the run-formatting remainder (Part A).
    assert_eq!(live.caps, MarkValue::On);
    assert_eq!(live.small_caps, MarkValue::On);
    assert_eq!(live.char_spacing, Some(40));

    // Reject-all restores the original StyleProps exactly (shape() cannot see this).
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        styled_props(&rejected, "Confidential"),
        base_props,
        "reject-all must restore the original run StyleProps (color/highlight/font)"
    );

    // Accept-all equals Direct apply on the full StyleProps.
    let mut accepted = tracked;
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        styled_props(&accepted, "Confidential"),
        styled_props(&direct, "Confidential"),
        "accept-all StyleProps must equal direct-apply StyleProps"
    );
}

/// paragraph-formatting: set alignment + spacing on a paragraph as a tracked
/// `w:pPrChange`, in place, without swapping the paragraph role.
///
/// `shape()` now renders `align`/`indent`/`spacing` (it zips canon blocks with
/// the view), so the generic harness already proves reversibility for the
/// fields this verb touches. We still assert directly on the IR for the fields
/// the fingerprint debug-formats but does not structurally guarantee, and to
/// document the contract: tracked → new value live; reject-all → original
/// value; accept-all == direct apply.
#[test]
fn set_paragraph_formatting_is_faithful() {
    let (base, ids) = doc_and_ids(&["A clause that should be centered."]);
    let block_id = NodeId::from(ids[0].as_str());
    let steps = vec![EditStep::SetParagraphFormatting {
        block_id: block_id.clone(),
        semantic_hash: None,
        patch: ParagraphFormattingPatch {
            align: Some(Alignment::Center),
            indent: None,
            spacing: Some(ParagraphSpacing {
                before: Some(240),
                after: Some(120),
                before_lines: None,
                after_lines: None,
                before_autospacing: None,
                after_autospacing: None,
                line: None,
                line_rule: None,
            }),
            borders: None,
            shading: None,
        },
        rationale: None,
    }];

    // Generic gate: reversibility (shape() now sees align/spacing),
    // accept==direct, non-shrinking opaque inventory.
    assert_fidelity("paragraph_formatting", &base, steps.clone());

    // Direct IR assertions on the first paragraph's pPr.
    fn first_para(canon: &CanonDoc) -> &ParagraphNode {
        match &canon.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("first block is not a paragraph"),
        }
    }

    let base_align = first_para(&base).align.clone();
    let base_spacing = first_para(&base).spacing.clone();

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;

    // Tracked edit set the new pPr live and recorded a pPrChange.
    {
        let p = first_para(&tracked);
        assert_eq!(p.align, Some(Alignment::Center));
        assert_eq!(p.spacing.as_ref().and_then(|s| s.before), Some(240));
        assert_eq!(p.spacing.as_ref().and_then(|s| s.after), Some(120));
        assert!(
            p.formatting_change.is_some(),
            "tracked mode must record a pPrChange"
        );
        // Role/style left untouched (this verb does not swap the role).
        assert_eq!(p.style_id, first_para(&base).style_id);
    }

    // Reject-all restores the original alignment + spacing exactly.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    {
        let p = first_para(&rejected);
        assert_eq!(p.align, base_align, "reject-all must restore alignment");
        assert_eq!(p.spacing, base_spacing, "reject-all must restore spacing");
        assert!(
            p.formatting_change.is_none(),
            "reject-all must drop the pPrChange record"
        );
    }

    // Accept-all equals Direct apply on the pPr.
    let mut accepted = tracked;
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    {
        let a = first_para(&accepted);
        let d = first_para(&direct);
        assert_eq!(a.align, d.align, "accept-all alignment must equal direct");
        assert_eq!(a.spacing, d.spacing, "accept-all spacing must equal direct");
        assert!(
            a.formatting_change.is_none() && d.formatting_change.is_none(),
            "neither accept-all nor direct leaves a pPrChange record"
        );
    }
}

/// paragraph-formatting (borders + shading): the same in-place `w:pPrChange`
/// verb now also carries `w:pBdr` / `w:shd`. Tracked sets new borders/shading
/// live plus a pPrChange; reject-all restores the original (absent)
/// borders/shading; accept-all equals direct apply. This pins the warm-up half
/// of the formatting-authoring foundation through the SAME verb seam the cell
/// exemplar mirrors.
#[test]
fn set_paragraph_formatting_borders_and_shading_is_faithful() {
    let (base, ids) = doc_and_ids(&["A clause to be boxed and shaded."]);
    let block_id = NodeId::from(ids[0].as_str());

    let borders = ParagraphBorders {
        top: Some(Border {
            style: BorderStyle::Single,
            color: Some("000000".to_string()),
            size: Some(4),
            space: Some(1),
            extra_attrs: Vec::new(),
        }),
        bottom: Some(Border {
            style: BorderStyle::Single,
            color: Some("000000".to_string()),
            size: Some(4),
            space: Some(1),
            extra_attrs: Vec::new(),
        }),
        left: None,
        right: None,
        between: None,
        bar: None,
    };
    let shading = Shading {
        fill: Some("FFFF00".to_string()),
        val: Some(ShadingPattern::Clear),
        color: Some("auto".to_string()),
        extra_attrs: Vec::new(),
    };

    let steps = vec![EditStep::SetParagraphFormatting {
        block_id: block_id.clone(),
        semantic_hash: None,
        patch: ParagraphFormattingPatch {
            align: None,
            indent: None,
            spacing: None,
            borders: Some(borders.clone()),
            shading: Some(shading.clone()),
        },
        rationale: None,
    }];

    assert_fidelity("paragraph_formatting_borders_shading", &base, steps.clone());

    fn first_para(canon: &CanonDoc) -> &ParagraphNode {
        match &canon.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("first block is not a paragraph"),
        }
    }

    let base_borders = first_para(&base).borders.clone();
    let base_shading = first_para(&base).shading.clone();

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    {
        let p = first_para(&tracked);
        assert_eq!(
            p.borders,
            Some(borders.clone()),
            "tracked sets new borders live"
        );
        assert_eq!(
            p.shading,
            Some(shading.clone()),
            "tracked sets new shading live"
        );
        assert!(
            p.formatting_change.is_some(),
            "tracked mode records a pPrChange"
        );
    }

    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    {
        let p = first_para(&rejected);
        assert_eq!(
            p.borders, base_borders,
            "reject-all restores original borders"
        );
        assert_eq!(
            p.shading, base_shading,
            "reject-all restores original shading"
        );
        assert!(
            p.formatting_change.is_none(),
            "reject-all drops the pPrChange"
        );
    }

    let mut accepted = tracked;
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    {
        let a = first_para(&accepted);
        let d = first_para(&direct);
        assert_eq!(a.borders, d.borders, "accept-all borders must equal direct");
        assert_eq!(a.shading, d.shading, "accept-all shading must equal direct");
        assert_eq!(a.borders, Some(borders), "accept-all keeps the new borders");
        assert_eq!(a.shading, Some(shading), "accept-all keeps the new shading");
    }
}

/// named-styles: apply a paragraph style (`w:pStyle`) as a tracked
/// `w:pPrChange`, in place, without touching the segment text.
///
/// `shape()` renders the block's `style_id` (from the view), so the generic
/// harness already proves reversibility for the field this verb changes: a
/// broken reject would leave the new style and the rejected shape would diverge
/// from base. We additionally assert directly on the IR to document the
/// contract (tracked → new style live + pPrChange; reject → previous style;
/// accept == direct).
#[test]
fn apply_style_is_faithful() {
    let (base, ids) = doc_and_ids(&["A clause that should become a heading."]);
    let block_id = NodeId::from(ids[0].as_str());
    let steps = vec![EditStep::ApplyStyle {
        block_id: block_id.clone(),
        semantic_hash: None,
        style_id: "Heading2".to_string(),
        rationale: None,
    }];

    // Generic gate: reversibility (shape() sees style_id), accept==direct,
    // non-shrinking opaque inventory.
    assert_fidelity("apply_style", &base, steps.clone());

    fn first_para(canon: &CanonDoc) -> &ParagraphNode {
        match &canon.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("first block is not a paragraph"),
        }
    }

    let base_style = first_para(&base).style_id.clone();

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    {
        let p = first_para(&tracked);
        assert_eq!(p.style_id.as_deref(), Some("Heading2"));
        assert!(
            p.formatting_change.is_some(),
            "tracked mode records a pPrChange"
        );
        assert_eq!(
            p.formatting_change.as_ref().unwrap().previous_style_id,
            base_style,
            "pPrChange records the prior style"
        );
    }

    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    {
        let p = first_para(&rejected);
        assert_eq!(
            p.style_id, base_style,
            "reject-all restores the previous style"
        );
        assert!(
            p.formatting_change.is_none(),
            "reject-all drops the pPrChange"
        );
    }

    let mut accepted = tracked;
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        first_para(&accepted).style_id,
        first_para(&direct).style_id,
        "accept-all style must equal direct apply"
    );
}

#[test]
fn insert_paragraph_is_faithful() {
    let (base, ids) = doc_and_ids(&["First", "Last"]);
    assert_fidelity(
        "insert",
        &base,
        vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from(ids[0].as_str()),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: text_content("Inserted middle paragraph"),
                restart_numbering: false,
                list: None,
            })],
        }],
    );
}

#[test]
fn delete_block_range_is_faithful() {
    let (base, ids) = doc_and_ids(&["Keep this", "Delete this", "Keep that"]);
    assert_fidelity(
        "delete",
        &base,
        vec![EditStep::DeleteBlockRange {
            from_block_id: NodeId::from(ids[1].as_str()),
            to_block_id: NodeId::from(ids[1].as_str()),
            rationale: None,
            expect: "Delete this".to_string(),
            semantic_hash: None,
        }],
    );
}

/// The "nasty" layered-edit case: a new edit layered onto a document that
/// already carries a tracked change. The pre-existing change must survive, and
/// reject-all of the doubly-edited document must return to the pristine original
/// (both changes reverted), while accept-all equals applying both directly.
#[test]
fn layered_edit_on_already_tracked_document() {
    let (base, ids) = doc_and_ids(&["First clause text", "Second clause text"]);

    // Edit 1 (tracked): rewrite the first block.
    let step1 = EditStep::ReplaceParagraphText {
        block_id: NodeId::from(ids[0].as_str()),
        rationale: None,
        replacement_role: None,
        expect: "First clause text".to_string(),
        semantic_hash: None,
        content: text_content("First clause, amended"),
    };
    let doc1 = apply_transaction(
        &base,
        &txn(vec![step1.clone()], MaterializationMode::TrackedChange),
    )
    .expect("edit 1")
    .0;

    // Edit 2 (tracked): bold a word in the *second* block.
    let step2 = EditStep::SetRunFormatting {
        block_id: NodeId::from(ids[1].as_str()),
        expect: "Second".to_string(),
        semantic_hash: None,
        marks: InlineMarkSet {
            bold: true,
            ..Default::default()
        },
        style: RunStyleEdit::default(),
        rationale: None,
    };
    let doc2 = apply_transaction(
        &doc1,
        &txn(vec![step2.clone()], MaterializationMode::TrackedChange),
    )
    .expect("edit 2 on already-tracked doc")
    .0;

    // Reject-all reverts BOTH changes back to the pristine original.
    let mut rejected = doc2.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        shape(&base),
        shape(&rejected),
        "reject-all of a layered edit must return to the pristine original"
    );

    // Accept-all equals applying both edits directly, in order.
    let mut accepted = doc2.clone();
    accept_all(&mut accepted);
    let direct1 = apply_transaction(&base, &txn(vec![step1], MaterializationMode::Direct))
        .expect("direct edit 1")
        .0;
    let direct2 = apply_transaction(&direct1, &txn(vec![step2], MaterializationMode::Direct))
        .expect("direct edit 2")
        .0;
    assert_eq!(
        shape(&accepted),
        shape(&direct2),
        "accept-all of a layered edit must equal applying both directly"
    );
}

/// fields-crossrefs: insert a new REF cross-reference field as a tracked
/// `w:fldSimple` insert.
///
/// The generic `shape()` harness proves the three structural invariants:
/// (1) reject-all drops the inserted field and reconstructs the original
/// (the field opaque disappears), (2) accept-all == direct apply (both keep
/// the field as a Normal opaque), (3) the opaque inventory is non-shrinking.
///
/// `shape()` renders an opaque span only as its `OpaqueKind` Debug, so the
/// REF instruction text and bookmark are NOT visible to it. We therefore add
/// direct IR assertions that the synthesized field carries the right
/// `FieldKind::Simple` + `FieldSemantic::Ref` + instruction text — the only
/// place those values are observable.
#[test]
fn insert_cross_reference_is_faithful() {
    let (base, ids) = doc_and_ids(&["See the Definitions section for details."]);
    let block_id = NodeId::from(ids[0].as_str());
    let spec = RefFieldSpec {
        kind: RefKind::Ref,
        bookmark: "Definitions".to_string(),
        insert_hyperlink: true,
        no_paragraph_number: false,
        paragraph_number_relative: false,
        paragraph_number_full: false,
        suppress_non_delimiter: false,
        above_below: false,
        format: FormatSwitches::default(),
    };
    let steps = vec![EditStep::InsertCrossReference {
        block_id: block_id.clone(),
        expect: "Definitions".to_string(),
        semantic_hash: None,
        spec: spec.clone(),
        rationale: None,
    }];

    // (a) Structural gate (reversibility, accept==direct, opaque inventory).
    assert_fidelity("insert_cross_reference", &base, steps.clone());

    // (b) Value-bearing IR assertions: the synthesized field opaque.
    fn find_ref_field(canon: &CanonDoc) -> Option<FieldData> {
        for tb in &canon.blocks {
            if let BlockNode::Paragraph(p) = &tb.block {
                for seg in &p.segments {
                    for inline in &seg.inlines {
                        if let InlineNode::OpaqueInline(o) = inline
                            && let OpaqueKind::Field(data) = &o.kind
                            && matches!(data.semantic, Some(FieldSemantic::Ref(_)))
                        {
                            return Some(data.clone());
                        }
                    }
                }
            }
        }
        None
    }

    // Base has no cross-reference field.
    assert!(
        find_ref_field(&base).is_none(),
        "base document must not already carry a REF field"
    );

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    let field = find_ref_field(&tracked).expect("tracked doc must carry the new REF field");
    assert_eq!(field.field_kind, FieldKind::Simple);
    // §17.16.5.45 — `REF <bookmark> \h`. result_text stays None (Word
    // recalculates the displayed reference on open).
    assert_eq!(
        field.instruction_text.as_deref(),
        Some("REF Definitions \\h")
    );
    assert_eq!(field.result_text, None);

    // Reject-all removes the inserted field entirely.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert!(
        find_ref_field(&rejected).is_none(),
        "reject-all must drop the inserted REF field"
    );

    // Accept-all keeps a field whose semantic spec equals the direct apply's.
    let mut accepted = tracked;
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        find_ref_field(&accepted).map(|d| d.semantic),
        find_ref_field(&direct).map(|d| d.semantic),
        "accept-all field semantic must equal direct-apply field semantic"
    );
}

/// A DOCX carrying a `word/numbering.xml` part with a single decimal list
/// (`numId=1`, two ilvls), plus body paragraphs. Each `(text, numbering)` tuple
/// is rendered either as a plain paragraph (`None`) or one with `w:numPr`
/// (`Some((num_id, ilvl))`). This is the fixture the numbering verb needs: the
/// plain `make_test_docx` has no numbering part, so a list-attach/detach edit
/// has no definitions to round-trip against.
fn make_numbered_docx(paras: &[(&str, Option<(u32, u32)>)]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for (text, numbering) in paras {
        document_xml.push_str("<w:p>");
        if let Some((num_id, ilvl)) = numbering {
            document_xml.push_str(&format!(
                r#"<w:pPr><w:numPr><w:ilvl w:val="{ilvl}"/><w:numId w:val="{num_id}"/></w:numPr></w:pPr>"#
            ));
        }
        document_xml.push_str(&format!(r#"<w:r><w:t>{text}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    // A minimal decimal list: abstractNumId=0 with two decimal levels, num
    // instance numId=1 → abstractNumId=0.
    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%2)"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(numbering_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Parse a numbered doc; return its canonical IR and block ids in order.
fn numbered_doc_and_ids(paras: &[(&str, Option<(u32, u32)>)]) -> (CanonDoc, Vec<String>) {
    let doc = Document::parse(&make_numbered_docx(paras)).expect("parse numbered");
    let ids = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();
    ((*doc.snapshot().canonical).clone(), ids)
}

/// The structural `(num_id, ilvl)` of the paragraph at `block_idx`, or `None` if
/// it carries no numbering. `shape()` is blind to `p.numbering`, so the
/// numbering verb's reversibility must be asserted directly on the IR.
fn para_numbering(canon: &CanonDoc, block_idx: usize) -> Option<(u32, u32)> {
    match &canon.blocks[block_idx].block {
        BlockNode::Paragraph(p) => p.numbering.as_ref().map(|n| (n.num_id, n.ilvl)),
        _ => panic!("block {block_idx} is not a paragraph"),
    }
}

/// lists-numbering: author a tracked paragraph numbering change (attach a list,
/// re-level, detach) as a `w:pPrChange` carrying the previous `w:numPr`.
///
/// `shape()` does NOT render `p.numbering`, so it cannot see whether reject-all
/// restored the original numbering. We therefore (a) run the generic harness —
/// it proves the segments/role/opaque inventory reverse — and (b) assert the
/// structural `(num_id, ilvl)` reversibility/accept==direct DIRECTLY on the IR.
fn assert_numbering_fidelity(
    label: &str,
    base: &CanonDoc,
    block_idx: usize,
    steps: Vec<EditStep>,
    expected_new: Option<(u32, u32)>,
) {
    // (a) generic structural gate.
    assert_fidelity(label, base, steps.clone());

    let base_num = para_numbering(base, block_idx);

    let tracked = apply_transaction(
        base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .unwrap()
    .0;
    assert_eq!(
        para_numbering(&tracked, block_idx),
        expected_new,
        "[{label}] tracked edit must set the requested numbering"
    );

    // Reject-all restores the original numbering exactly (shape() cannot see this).
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        para_numbering(&rejected, block_idx),
        base_num,
        "[{label}] reject-all must restore the original numbering"
    );

    // Accept-all equals Direct apply on the structural numbering.
    let mut accepted = tracked;
    accept_all(&mut accepted);
    let direct = apply_transaction(base, &txn(steps, MaterializationMode::Direct))
        .unwrap()
        .0;
    assert_eq!(
        para_numbering(&accepted, block_idx),
        para_numbering(&direct, block_idx),
        "[{label}] accept-all numbering must equal direct-apply numbering"
    );
}

#[test]
fn set_paragraph_numbering_attach_list_is_faithful() {
    // Plain paragraph → attach decimal list (numId=1, ilvl=0). Reject restores
    // None; this exercises previous_numbering_explicitly_absent + the reject
    // restore path.
    let (base, ids) = numbered_doc_and_ids(&[("First item", None), ("Second", None)]);
    assert_numbering_fidelity(
        "numbering_attach",
        &base,
        0,
        vec![EditStep::SetParagraphNumbering {
            block_id: NodeId::from(ids[0].as_str()),
            semantic_hash: None,
            change: NumberingChange::SetList {
                num_id: 1,
                ilvl: 0,
                restart: false,
                synthesized_text: "1.".to_string(),
                is_bullet: false,
            },
            rationale: None,
        }],
        Some((1, 0)),
    );
}

#[test]
fn set_paragraph_numbering_detach_list_is_faithful() {
    // Numbered paragraph → Remove. Reject restores the original (1, 0).
    let (base, ids) = numbered_doc_and_ids(&[("Item one", Some((1, 0))), ("Plain", None)]);
    assert_numbering_fidelity(
        "numbering_detach",
        &base,
        0,
        vec![EditStep::SetParagraphNumbering {
            block_id: NodeId::from(ids[0].as_str()),
            semantic_hash: None,
            change: NumberingChange::Remove,
            rationale: None,
        }],
        None,
    );
}

#[test]
fn set_paragraph_numbering_set_level_is_faithful() {
    // Numbered paragraph at ilvl=0 → indent to ilvl=1. Reject restores ilvl=0.
    let (base, ids) = numbered_doc_and_ids(&[("Item one", Some((1, 0)))]);
    assert_numbering_fidelity(
        "numbering_set_level",
        &base,
        0,
        vec![EditStep::SetParagraphNumbering {
            block_id: NodeId::from(ids[0].as_str()),
            semantic_hash: None,
            change: NumberingChange::SetLevel {
                ilvl: 1,
                synthesized_text: "(a)".to_string(),
                is_bullet: false,
            },
            rationale: None,
        }],
        Some((1, 1)),
    );
}

#[test]
fn list_op_indent_is_faithful() {
    // ilvl 0 -> 1 via the first-class Indent op (no caller-supplied label).
    let (base, ids) = numbered_doc_and_ids(&[("Item one", Some((1, 0)))]);
    assert_numbering_fidelity(
        "list_indent",
        &base,
        0,
        vec![EditStep::SetParagraphNumbering {
            block_id: NodeId::from(ids[0].as_str()),
            semantic_hash: None,
            change: NumberingChange::Indent,
            rationale: None,
        }],
        Some((1, 1)),
    );
}

#[test]
fn list_op_outdent_is_faithful() {
    // ilvl 1 -> 0 via the first-class Outdent op.
    let (base, ids) = numbered_doc_and_ids(&[("Item one", Some((1, 1)))]);
    assert_numbering_fidelity(
        "list_outdent",
        &base,
        0,
        vec![EditStep::SetParagraphNumbering {
            block_id: NodeId::from(ids[0].as_str()),
            semantic_hash: None,
            change: NumberingChange::Outdent,
            rationale: None,
        }],
        Some((1, 0)),
    );
}

#[test]
fn list_op_restart_is_faithful() {
    // restart_numbering flips true; (num_id, ilvl) unchanged so para_numbering
    // reports the same (1, 0) — the harness still proves reject restores the
    // base restart intent via the generic shape gate + numbering equality.
    let (base, ids) = numbered_doc_and_ids(&[("Item one", Some((1, 0)))]);
    assert_numbering_fidelity(
        "list_restart",
        &base,
        0,
        vec![EditStep::SetParagraphNumbering {
            block_id: NodeId::from(ids[0].as_str()),
            semantic_hash: None,
            change: NumberingChange::Restart,
            rationale: None,
        }],
        Some((1, 0)),
    );
}

// NOTE: SetType (bullet<->numbered kind swap) fidelity is covered by the verb
// unit tests in `edit/verbs/numbering.rs`. A faithful integration case needs a
// SECOND list definition of the target kind in the fixture (re-pointing at the
// SAME numId is a structural no-op and is correctly refused). The minimal
// `make_numbered_docx` fixture carries only numId=1, so the kind-swap path is
// exercised at the unit level until a two-list fixture exists.

/// A 2x2-cell table DOCX (no merges, no header, default formatting) followed by
/// a trailing empty paragraph (Word requires a paragraph after a table). The
/// base the `tables-merged` `replace(table)` operates on.
fn make_table_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">B</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">C</w:t></w:r></w:p></w:tc><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">D</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;

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

/// tables-merged: `replace(table)` that authors merged cells (gridSpan +
/// vMerge) and a header row, as tracked changes.
///
/// The standing `shape()` fingerprint is BLIND to table merge/header state: it
/// renders a `BlockRole::Table` as just `[table|...]` and does not descend into
/// rows/cells, so a broken reversal of grid_span/v_merge/is_header would still
/// pass the generic gate. We therefore (a) run the generic harness — it proves
/// the structural row/cell diff reverses and the opaque inventory is
/// non-shrinking — and (b) assert reversibility/accept==direct DIRECTLY on the
/// IR (`TableCellNode.grid_span`/`.v_merge`, `TableRowNode.is_header`), the only
/// place these values are observable.
#[test]
fn replace_table_merged_cells_is_faithful() {
    let doc = Document::parse(&make_table_docx()).expect("parse table docx");
    let base = doc.snapshot().canonical.clone();
    // First block is the table.
    let table_id = match &base.blocks[0].block {
        BlockNode::Table(t) => t.id.clone(),
        other => panic!("expected first block to be a table, got {other:?}"),
    };

    // Replacement: row 0 becomes a single header cell spanning both columns
    // (gridSpan 2); row 1's first column is the vMerge restart anchor and... a
    // 2-row vertical merge needs two rows in the same column. Keep it concrete:
    // row 0 = one gridSpan-2 header cell; row 1 = two normal cells whose first
    // is a vMerge restart, row 2 = two cells whose first is the continue.
    fn cell(text: &str, merge_h: Option<u32>, merge_v: Option<VerticalMergeSpec>) -> TableCellSpec {
        TableCellSpec {
            content: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: text_content(text),
                restart_numbering: false,
                list: None,
            })],
            merge_h,
            merge_v,
            formatting: None,
        }
    }

    let replacement = TableBlockSpec {
        formatting: None,
        rows: vec![
            // Header row, one cell spanning both columns.
            TableRowSpec {
                cells: vec![cell("Header", Some(2), None)],
                is_header: true,
                height: None,
                height_rule: None,
            },
            // Body row: left cell starts a vertical merge.
            TableRowSpec {
                cells: vec![
                    cell("Left", None, Some(VerticalMergeSpec::Restart)),
                    cell("B2", None, None),
                ],
                is_header: false,
                height: None,
                height_rule: None,
            },
            // Body row: left cell continues the vertical merge.
            TableRowSpec {
                cells: vec![
                    cell("Left", None, Some(VerticalMergeSpec::Continue)),
                    cell("D2", None, None),
                ],
                is_header: false,
                height: None,
                height_rule: None,
            },
        ],
    };

    let steps = vec![EditStep::ReplaceTable {
        block_id: table_id.clone(),
        rationale: None,
        semantic_hash: None,
        replacement,
    }];

    // (a) Structural gate: row/cell diff reverses, opaque inventory non-shrinking.
    assert_fidelity("tables_merged", &base, steps.clone());

    // (b) Value-bearing IR assertions on merge/header state (shape() is blind).
    //
    // Helper: the merge/header fingerprint of the first table block, as the
    // sequence of (is_header, [ (grid_span, v_merge) per cell ]) per row.
    fn table_merge_shape(canon: &CanonDoc) -> Vec<(bool, Vec<(u32, VerticalMerge)>)> {
        match &canon.blocks[0].block {
            BlockNode::Table(t) => t
                .rows
                .iter()
                .map(|r| {
                    (
                        r.is_header,
                        r.cells
                            .iter()
                            .map(|c| (c.grid_span, c.v_merge.clone()))
                            .collect(),
                    )
                })
                .collect(),
            other => panic!("expected table, got {other:?}"),
        }
    }

    let base_shape = table_merge_shape(&base);

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;

    // Accept-all adopts the new merge/header state.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    let accepted_shape = table_merge_shape(&accepted);
    let direct_shape = table_merge_shape(&direct);
    assert_eq!(
        accepted_shape, direct_shape,
        "accept-all merge/header state must equal direct apply"
    );
    // The intended target really did carry the merges + header (not a no-op).
    assert!(direct_shape[0].0, "row 0 must be a header row after accept");
    assert_eq!(
        direct_shape[0].1[0].0, 2,
        "row 0 cell 0 must span 2 columns (gridSpan)"
    );
    assert_eq!(
        direct_shape[1].1[0].1,
        VerticalMerge::Restart,
        "row 1 cell 0 must be the vMerge restart anchor"
    );
    assert_eq!(
        direct_shape[2].1[0].1,
        VerticalMerge::Continue,
        "row 2 cell 0 must be the vMerge continuation"
    );

    // Reject-all restores the ORIGINAL merge/header state exactly (shape() blind).
    let mut rejected = tracked;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        table_merge_shape(&rejected),
        base_shape,
        "reject-all must restore the original table merge/header state"
    );
}

/// A 1×1 table whose single row carries an EXPLICIT height (`w:trHeight`) so a
/// `SetRowFormatting` reject must restore that prior value (not just `None`).
fn make_row_height_table_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="360" w:hRule="atLeast"/></w:trPr><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
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

/// `SetRowFormatting`: a tracked `w:trPrChange` that re-heights one row. The
/// generic harness proves the opaque inventory is non-shrinking and the
/// structural diff reverses; but `shape()` is BLIND to row height, so we ALSO
/// assert the height/height_rule directly on the IR — the only place these
/// values are observable. Reject restores the prior height (360 / atLeast),
/// accept adopts the new height (720 / exact) and equals a direct apply.
#[test]
fn set_row_formatting_is_faithful() {
    let doc = Document::parse(&make_row_height_table_docx()).expect("parse row-height docx");
    let base = doc.snapshot().canonical.clone();
    let table_id = match &base.blocks[0].block {
        BlockNode::Table(t) => t.id.clone(),
        other => panic!("expected first block to be a table, got {other:?}"),
    };

    // The base row really carries the prior height (not a no-op base).
    fn row0_height(canon: &CanonDoc) -> (Option<u32>, Option<HeightRule>) {
        match &canon.blocks[0].block {
            BlockNode::Table(t) => (t.rows[0].height, t.rows[0].height_rule.clone()),
            other => panic!("expected table, got {other:?}"),
        }
    }
    assert_eq!(
        row0_height(&base),
        (Some(360), Some(HeightRule::AtLeast)),
        "base row must carry the explicit prior height"
    );

    let steps = vec![EditStep::SetRowFormatting {
        block_id: table_id,
        row_index: 0,
        semantic_hash: None,
        patch: RowFormattingPatch {
            height: Some(720),
            height_rule: Some(HeightRule::Exact),
        },
        rationale: None,
    }];

    // (a) Standing harness: opaque inventory non-shrinking, structural diff ok.
    assert_fidelity("set_row_formatting", &base, steps.clone());

    // (b) Value-bearing IR assertions (shape() is blind to row height).
    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;

    // Accept-all adopts the new height and equals direct apply.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        row0_height(&accepted),
        (Some(720), Some(HeightRule::Exact)),
        "accept-all must adopt the new row height"
    );
    assert_eq!(
        row0_height(&accepted),
        row0_height(&direct),
        "accept-all row height must equal direct apply"
    );

    // Reject-all restores the ORIGINAL height exactly (shape() blind).
    let mut rejected = tracked;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        row0_height(&rejected),
        (Some(360), Some(HeightRule::AtLeast)),
        "reject-all must restore the original row height"
    );
}

/// find-replace-all: the pure planner composes `ReplaceParagraphText` steps.
/// The standing harness proves the composed plan is faithful: reject-all
/// reconstructs the original via `shape()`, accept-all equals direct apply, and
/// the opaque inventory does not shrink. We derive the steps from
/// `plan_find_replace_all` so the planner's section-interleaving (text +
/// preserved-inline refs) is what gets gated, not a hand-built step.
#[test]
fn find_replace_all_is_faithful() {
    let (base, _ids) = doc_and_ids(&[
        "The cat sat on the cat mat.",
        "A second cat appears here.",
        "No felines in this line.",
    ]);

    let plan = plan_find_replace_all(
        &base,
        &FindReplaceOptions {
            needle: "cat".to_string(),
            replacement: "lion".to_string(),
            scope: FindReplaceScope::BodyOnly,
            case_sensitive: true,
            whole_word: false,
            on_barrier_match: BarrierPolicy::Skip,
        },
    )
    .expect("plan");

    // Two paragraphs match; the third produces no step.
    assert_eq!(plan.len(), 2, "only the matching paragraphs get a step");

    assert_fidelity("find_replace_all", &base, plan);
}

// ─── Bookmark verb (zero-width decorations; shape() is blind to decorations) ──
//
// Bookmarks are NOT tracked content: they are Normal `Decoration{Bookmark}`
// pairs that do not change visible text. So:
//   - the generic `shape()` is identical before/after (text-identity invariant,
//     proved here directly);
//   - the opaque inventory is non-shrinking (no images/fields touched);
//   - the right notion of "reversibility" is the DECORATION inventory, asserted
//     directly on the IR (Insert/Rename are non-shrinking; Remove drops exactly
//     the named pair and leaves every other decoration byte-identical).

/// Every bookmark `(local_name, w:id, Option<name>)` triple in the document, in
/// document order. Parses each decoration's `raw_xml` to read the bytes that ARE
/// the bookmark.
fn bookmark_markers(canon: &CanonDoc) -> Vec<(String, Option<String>, Option<String>)> {
    fn attr(xml: &str, attr: &str) -> Option<String> {
        let needle = format!("{attr}=\"");
        let start = xml.find(&needle)? + needle.len();
        let rest = &xml[start..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }
    let mut out = Vec::new();
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Decoration(d) = inline
                        && d.kind == DecorationType::Bookmark
                        && let Some(raw) = &d.raw_xml
                    {
                        let xml = String::from_utf8_lossy(raw);
                        let local = if xml.contains("bookmarkStart") {
                            "bookmarkStart"
                        } else if xml.contains("bookmarkEnd") {
                            "bookmarkEnd"
                        } else {
                            continue;
                        };
                        out.push((local.to_string(), attr(&xml, "w:id"), attr(&xml, "w:name")));
                    }
                }
            }
        }
    }
    out
}

/// All decoration `raw_xml` blobs in document order (for byte-identity checks).
fn decoration_blobs(canon: &CanonDoc) -> Vec<Option<Vec<u8>>> {
    let mut out = Vec::new();
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Decoration(d) = inline {
                        out.push(d.raw_xml.clone());
                    }
                }
            }
        }
    }
    out
}

#[test]
fn insert_bookmark_is_faithful() {
    let (base, ids) = doc_and_ids(&["The Confidential Information clause."]);
    let block_id = NodeId::from(ids[0].as_str());
    let steps = vec![EditStep::InsertBookmark {
        block_id: block_id.clone(),
        expect: "Confidential Information".to_string(),
        semantic_hash: None,
        name: "DefTerm".to_string(),
        rationale: None,
    }];

    // Generic gate: text identity (shape() blind to decorations => unchanged),
    // accept==direct, non-shrinking opaque inventory.
    assert_fidelity("insert_bookmark", &base, steps.clone());

    let tracked = apply_transaction(&base, &txn(steps, MaterializationMode::TrackedChange))
        .expect("tracked apply")
        .0;

    // Decoration inventory is non-shrinking: a start+end pair was ADDED.
    let before = bookmark_markers(&base);
    let after = bookmark_markers(&tracked);
    assert_eq!(before.len(), 0, "base has no bookmarks");
    assert_eq!(after.len(), 2, "insert adds exactly a start + end");
    assert_eq!(after[0].0, "bookmarkStart");
    assert_eq!(after[1].0, "bookmarkEnd");
    assert_eq!(after[0].2.as_deref(), Some("DefTerm"));
    // start/end share one id.
    assert_eq!(after[0].1, after[1].1);
}

#[test]
fn rename_bookmark_is_faithful() {
    let (base, ids) = doc_and_ids(&["alpha beta gamma"]);
    let block_id = NodeId::from(ids[0].as_str());

    let inserted = apply_transaction(
        &base,
        &txn(
            vec![EditStep::InsertBookmark {
                block_id: block_id.clone(),
                expect: "beta".to_string(),
                semantic_hash: None,
                name: "OldName".to_string(),
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("insert")
    .0;

    let before = bookmark_markers(&inserted);
    let id_before = before[0].1.clone();

    let renamed = apply_transaction(
        &inserted,
        &txn(
            vec![EditStep::RenameBookmark {
                block_id,
                old_name: "OldName".to_string(),
                new_name: "NewName".to_string(),
                semantic_hash: None,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("rename")
    .0;

    let after = bookmark_markers(&renamed);
    // Non-shrinking: still exactly a start + end.
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].2.as_deref(), Some("NewName"), "name changed");
    assert_eq!(after[0].1, id_before, "rename must NOT touch the w:id");
    assert_eq!(after[1].1, id_before, "end still pairs");
}

#[test]
fn remove_bookmark_drops_exactly_the_named_pair() {
    let (base, ids) = doc_and_ids(&["one two three four"]);
    let block_id = NodeId::from(ids[0].as_str());

    // Two bookmarks.
    let inserted = apply_transaction(
        &base,
        &txn(
            vec![
                EditStep::InsertBookmark {
                    block_id: block_id.clone(),
                    expect: "two".to_string(),
                    semantic_hash: None,
                    name: "Keep".to_string(),
                    rationale: None,
                },
                EditStep::InsertBookmark {
                    block_id: block_id.clone(),
                    expect: "four".to_string(),
                    semantic_hash: None,
                    name: "Drop".to_string(),
                    rationale: None,
                },
            ],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("two inserts")
    .0;

    // Record the surviving bookmark's exact bytes BEFORE removal.
    let keep_blob = decoration_blobs(&inserted)
        .into_iter()
        .flatten()
        .find(|b| String::from_utf8_lossy(b).contains("w:name=\"Keep\""))
        .expect("Keep start present");

    let removed = apply_transaction(
        &inserted,
        &txn(
            vec![EditStep::RemoveBookmark {
                block_id,
                name: "Drop".to_string(),
                semantic_hash: None,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("remove")
    .0;

    let after = bookmark_markers(&removed);
    assert_eq!(after.len(), 2, "exactly Keep's start+end remain");
    assert!(
        after.iter().all(|m| m.2.as_deref() != Some("Drop")),
        "Drop fully removed"
    );
    // The kept bookmark's start bytes are byte-identical (untouched by the remove).
    let keep_after = decoration_blobs(&removed)
        .into_iter()
        .flatten()
        .find(|b| String::from_utf8_lossy(b).contains("w:name=\"Keep\""))
        .expect("Keep survives");
    assert_eq!(
        keep_after, keep_blob,
        "untouched bookmark must be byte-identical"
    );

    // Removal must not have changed the visible text (shape() identity).
    assert_eq!(
        shape(&base),
        shape(&removed),
        "removing a zero-width bookmark must not change the document shape"
    );
}

// ─── Image attribute edits (Part B) ─────────────────────────────────────────

/// Fetch the current `raw_xml` of the first drawing in a doc, as a String.
fn drawing_raw_xml(canon: &CanonDoc) -> String {
    let (_, _, raw) = first_drawing(canon);
    String::from_utf8(raw).expect("raw_xml is utf-8")
}

/// set-image-attributes: resize + set alt text on an opaque drawing, in place.
///
/// `SetImageAttributes` is a DIRECT, UNTRACKED attribute edit (like
/// `SetHyperlinkAttr`) — OOXML has no tracked-change envelope for opaque-drawing
/// display attributes, so there is no reject-able delta. The fidelity contract
/// is therefore the untracked-verb form:
///   - tracked-mode result == direct-mode result (no tracked envelope created);
///   - accept-all and reject-all both leave the edited raw_xml (no-ops on it);
///   - the edited raw_xml carries the new wp:extent cx/cy and wp:docPr descr,
///     while the inner a:ext (the graphic frame's own box) is untouched;
///   - the opaque-anchor inventory is non-shrinking.
///
/// `shape()` is blind to opaque raw_xml, so every assertion is direct on the IR.
#[test]
fn set_image_attributes_is_faithful() {
    let bytes = make_drawing_docx(100, 200, "old alt");
    let base = Document::parse(&bytes)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, base_raw) = first_drawing(&base);
    let base_raw_str = String::from_utf8(base_raw).unwrap();
    // Baseline carries the original extent + descr, and the inner a:ext box.
    assert!(base_raw_str.contains(r#"cx="100""#));
    assert!(base_raw_str.contains(r#"descr="old alt""#));
    assert!(base_raw_str.contains(r#"cx="999""#));

    let steps = vec![EditStep::SetImageAttributes {
        block_id,
        drawing_id: drawing_id.clone(),
        semantic_hash: None,
        resize: Some(ImageResize {
            cx_emu: 4242,
            cy_emu: 5353,
        }),
        alt_text: Some(Some("new alt".to_string())),
        rationale: None,
    }];

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;

    // Untracked: tracked-mode and direct-mode produce identical raw_xml.
    assert_eq!(
        drawing_raw_xml(&tracked),
        drawing_raw_xml(&direct),
        "image attribute edit is untracked; tracked and direct modes must match"
    );

    let edited = drawing_raw_xml(&tracked);
    // wp:extent resized...
    assert!(
        edited.contains(r#"cx="4242""#),
        "wp:extent cx must be resized"
    );
    assert!(
        edited.contains(r#"cy="5353""#),
        "wp:extent cy must be resized"
    );
    // ...alt text set...
    assert!(
        edited.contains(r#"descr="new alt""#),
        "wp:docPr descr must be set"
    );
    // ...and the inner a:ext (graphic-frame box) is NOT the resize target.
    assert!(
        edited.contains(r#"cx="999""#),
        "inner a:ext must be untouched"
    );
    assert!(!edited.contains(r#"descr="old alt""#));

    // accept-all and reject-all are both no-ops on an untracked attribute edit.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    assert_eq!(
        drawing_raw_xml(&accepted),
        edited,
        "accept-all must not alter the edit"
    );
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        drawing_raw_xml(&rejected),
        edited,
        "reject-all must not alter the edit"
    );

    // Non-shrinking opaque inventory.
    let before = anchor_ids(&base);
    let after = anchor_ids(&tracked);
    for id in &before {
        assert!(
            after.contains(id),
            "opaque anchor '{id}' was dropped by the image edit"
        );
    }

    // content_hash tracks the edited raw_xml (changed from the base).
    let (_, _, edited_raw) = first_drawing(&tracked);
    assert_ne!(
        edited_raw,
        base_raw_str.into_bytes(),
        "raw_xml must have changed"
    );
}

/// Clearing alt text (three-state `Some(None)`) removes the @descr attribute
/// entirely rather than writing an empty string.
#[test]
fn set_image_attributes_clears_alt_text() {
    let bytes = make_drawing_docx(10, 20, "has alt");
    let base = Document::parse(&bytes)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);

    let steps = vec![EditStep::SetImageAttributes {
        block_id,
        drawing_id,
        semantic_hash: None,
        resize: None,
        alt_text: Some(None), // clear
        rationale: None,
    }];
    let edited = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("apply")
        .0;
    let raw = drawing_raw_xml(&edited);
    assert!(
        !raw.contains("descr="),
        "clearing alt text must remove @descr entirely"
    );
}

// ─── Image LAYOUT edits (crop / position / wrap) ─────────────────────────────

/// Build a DOCX whose single paragraph hosts an **inline** drawing with a real
/// `pic:blipFill` (so crop has its `a:srcRect` target) but NO `wp:anchor` (so
/// position/wrap are unreachable — the anchor-gate fires).
fn make_inline_picture_docx() -> Vec<u8> {
    let drawing = r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:extent cx="100" cy="200"/><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="Picture 1"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
    docx_with_drawing(drawing)
}

/// Build a DOCX whose single paragraph hosts a **floating (anchored)** drawing —
/// so position and wrap are reachable.
fn make_anchored_picture_docx() -> Vec<u8> {
    let drawing = r#"<w:drawing><wp:anchor xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="0" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="100" cy="200"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="Picture 1"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing>"#;
    docx_with_drawing(drawing)
}

/// Wrap a `<w:drawing>` fragment into a single-paragraph DOCX with an image rel.
fn docx_with_drawing(drawing: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r>{drawing}</w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
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
        zip.start_file("word/media/image1.png", opts).unwrap();
        zip.write_all(b"\x89PNG\r\n\x1a\n-fake-image-").unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Crop on an INLINE drawing: `SetImageLayout` is direct/untracked (like
/// `SetImageAttributes`), so tracked-mode == direct-mode, accept-all and
/// reject-all are both no-ops on the edited raw_xml, the `a:srcRect` insets are
/// present, and the opaque inventory does not shrink.
#[test]
fn set_image_layout_crop_is_faithful() {
    let bytes = make_inline_picture_docx();
    let base = Document::parse(&bytes)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);

    let steps = vec![EditStep::SetImageLayout {
        block_id,
        drawing_id: drawing_id.clone(),
        semantic_hash: None,
        patch: ImageLayoutPatch {
            crop: Some(ImageCrop {
                left: Some(10_000),
                top: Some(20_000),
                right: Some(30_000),
                bottom: Some(40_000),
            }),
            ..Default::default()
        },
        rationale: None,
    }];

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;

    // Untracked: tracked-mode and direct-mode produce identical raw_xml.
    assert_eq!(
        drawing_raw_xml(&tracked),
        drawing_raw_xml(&direct),
        "image layout edit is untracked; tracked and direct modes must match"
    );

    let edited = drawing_raw_xml(&tracked);
    assert!(
        edited.contains(r#"srcRect"#),
        "a:srcRect must be present: {edited}"
    );
    assert!(edited.contains(r#"l="10000""#), "{edited}");
    assert!(edited.contains(r#"t="20000""#));
    assert!(edited.contains(r#"r="30000""#));
    assert!(edited.contains(r#"b="40000""#));

    // accept-all and reject-all are both no-ops on an untracked attribute edit.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    assert_eq!(
        drawing_raw_xml(&accepted),
        edited,
        "accept-all must not alter the edit"
    );
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        drawing_raw_xml(&rejected),
        edited,
        "reject-all must not alter the edit"
    );

    // Non-shrinking opaque inventory.
    let before = anchor_ids(&base);
    let after = anchor_ids(&tracked);
    for id in &before {
        assert!(
            after.contains(id),
            "opaque anchor '{id}' was dropped by the layout edit"
        );
    }
}

/// Position + wrap on an ANCHORED drawing: reachable, untracked, accept==reject
/// no-op, attributes present, ordering preserved (positionH before extent, wrap
/// before docPr).
#[test]
fn set_image_layout_position_and_wrap_on_anchor_is_faithful() {
    let bytes = make_anchored_picture_docx();
    let base = Document::parse(&bytes)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);

    let steps = vec![EditStep::SetImageLayout {
        block_id,
        drawing_id,
        semantic_hash: None,
        patch: ImageLayoutPatch {
            position_h: Some(ImagePositionAxis::Offset {
                relative_from: "page".to_string(),
                offset_emu: 914_400,
            }),
            position_v: Some(ImagePositionAxis::Align {
                relative_from: "margin".to_string(),
                align: "center".to_string(),
            }),
            wrap: Some(ImageWrapType::Square),
            ..Default::default()
        },
        rationale: None,
    }];

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        drawing_raw_xml(&tracked),
        drawing_raw_xml(&direct),
        "untracked: modes must match"
    );

    let edited = drawing_raw_xml(&tracked);
    assert!(edited.contains(r#"relativeFrom="page""#), "{edited}");
    assert!(edited.contains("914400"), "{edited}");
    assert!(edited.contains("center"), "{edited}");
    assert!(edited.contains("wrapSquare"), "{edited}");
    assert!(!edited.contains("wrapNone"), "old wrap replaced: {edited}");
    // Schema order preserved.
    assert!(edited.find("positionH").unwrap() < edited.find("extent").unwrap());
    assert!(edited.find("wrapSquare").unwrap() < edited.find("docPr").unwrap());

    // accept/reject are no-ops.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    assert_eq!(drawing_raw_xml(&accepted), edited);
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(drawing_raw_xml(&rejected), edited);
}

/// Position/wrap on an INLINE drawing fail loud (`ImageLayoutRequiresAnchor`) —
/// no silent skip, and the document is left untouched.
#[test]
fn set_image_layout_position_on_inline_fails_loud() {
    let bytes = make_inline_picture_docx();
    let base = Document::parse(&bytes)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);

    let err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetImageLayout {
                block_id,
                drawing_id,
                semantic_hash: None,
                patch: ImageLayoutPatch {
                    wrap: Some(ImageWrapType::Tight),
                    ..Default::default()
                },
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .unwrap_err();
    assert!(
        matches!(err, EditError::ImageLayoutRequiresAnchor { .. }),
        "expected ImageLayoutRequiresAnchor, got {err:?}"
    );
}

/// `WrapBlocksInContentControl` is a DIRECT, UNTRACKED structural wrap (like the
/// inline `WrapInContentControl` and `SetImageAttributes`): OOXML has no
/// `w:sdtChange` envelope for a block-level `w:sdt`, so there is no reject-able
/// delta. The fidelity contract is therefore the untracked-verb form:
///   - tracked-mode result == direct-mode result (no tracked envelope created);
///   - accept-all and reject-all both leave the wrapped doc (no-ops on the wrap),
///     so on the serialized markup accept-all == reject-all == the wrapped doc,
///     and the `w:sdt` survives both with no `w:ins`/`w:del`/`w:sdtChange`;
///   - the wrapped blocks' content/opaque inventory is non-shrinking (the wrap
///     adds an envelope, it never drops inner content).
#[test]
fn wrap_blocks_in_content_control_is_faithful() {
    let bytes = make_test_docx(&["First wrapped block.", "Second wrapped block.", "Outside."]);
    let parsed = Document::parse(&bytes).expect("parse");
    let base = parsed.snapshot().canonical.clone();
    let start = match &base.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("expected a paragraph"),
    };
    let end = match &base.blocks[1].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("expected a paragraph"),
    };

    let steps = vec![EditStep::WrapBlocksInContentControl {
        start_block_id: start,
        end_block_id: end,
        spec: SdtSpec {
            tag: Some("clause".to_string()),
            alias: Some("Clause".to_string()),
            control: SdtControl::RichText,
            binding: None,
        },
        rationale: None,
    }];

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;

    // Untracked: tracked-mode and direct-mode produce identical IR (same wrap
    // marker, same blocks). `shape()` is blind to the wrap, so compare the wrap
    // markers directly too.
    assert_eq!(
        shape(&tracked),
        shape(&direct),
        "untracked wrap: tracked == direct shape"
    );
    assert_eq!(
        tracked.blocks[0].block_sdt_wrap, direct.blocks[0].block_sdt_wrap,
        "untracked wrap: tracked == direct wrap marker"
    );
    let wrap = tracked.blocks[0]
        .block_sdt_wrap
        .as_ref()
        .expect("the wrap marker lives on the first block");
    assert_eq!(wrap.span, 2, "the wrap spans both blocks");

    // accept-all and reject-all are both no-ops on the wrap marker.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        accepted.blocks[0].block_sdt_wrap, tracked.blocks[0].block_sdt_wrap,
        "accept-all must not alter the wrap"
    );
    assert_eq!(
        rejected.blocks[0].block_sdt_wrap, tracked.blocks[0].block_sdt_wrap,
        "reject-all must not alter the wrap (no w:sdtChange to undo)"
    );

    // On the serialized markup, accept-all == reject-all == the wrapped doc, with
    // the w:sdt present under both and no tracked envelope.
    let edited_doc = parsed
        .apply(&txn(
            vec![EditStep::WrapBlocksInContentControl {
                start_block_id: match &base.blocks[0].block {
                    BlockNode::Paragraph(p) => p.id.clone(),
                    _ => unreachable!(),
                },
                end_block_id: match &base.blocks[1].block {
                    BlockNode::Paragraph(p) => p.id.clone(),
                    _ => unreachable!(),
                },
                spec: SdtSpec {
                    tag: Some("clause".to_string()),
                    alias: Some("Clause".to_string()),
                    control: SdtControl::RichText,
                    binding: None,
                },
                rationale: None,
            }],
            MaterializationMode::Direct,
        ))
        .expect("apply on Document");
    let ser = |d: &Document| -> String {
        let archive = stemma::docx::DocxArchive::read(
            &d.serialize(&ExportOptions::default()).expect("serialize"),
        )
        .expect("read");
        String::from_utf8(archive.get("word/document.xml").expect("doc.xml").to_vec())
            .expect("utf8")
    };
    let wrapped_xml = ser(&edited_doc);
    let accept_xml = ser(&edited_doc.project(Resolution::AcceptAll).expect("accept"));
    let reject_xml = ser(&edited_doc.project(Resolution::RejectAll).expect("reject"));
    assert_eq!(accept_xml, wrapped_xml, "serialized accept-all == wrapped");
    assert_eq!(reject_xml, wrapped_xml, "serialized reject-all == wrapped");
    for (label, xml) in [
        ("wrapped", &wrapped_xml),
        ("accept", &accept_xml),
        ("reject", &reject_xml),
    ] {
        assert!(
            xml.contains("<w:sdt>") && xml.contains("<w:sdtContent>"),
            "[{label}] block-level w:sdt must survive"
        );
        assert!(
            !xml.contains("w:sdtChange"),
            "[{label}] no w:sdtChange envelope"
        );
        assert!(
            !xml.contains("<w:ins") && !xml.contains("<w:del"),
            "[{label}] not a tracked change"
        );
    }

    // Non-shrinking content: every base paragraph's text survives inside the doc.
    for needle in ["First wrapped block.", "Second wrapped block.", "Outside."] {
        assert!(
            wrapped_xml.contains(needle),
            "content '{needle}' must survive the wrap"
        );
    }
}

/// `WrapInContentControl` with a `data_binding` is the data-bound inline wrap.
/// Like the plain inline wrap it is UNTRACKED (no `w:sdtChange`), so the
/// standing fidelity invariants are stated at the untracked layer:
///   - tracked-mode IR == direct-mode IR (the mode does not change behavior);
///   - on the serialized markup, accept-all == reject-all == the bound doc, and
///     the `w:sdt` + its `w:dataBinding` survive both with no `w:ins`/`w:del`/
///     `w:sdtChange`;
///   - the backing `customXml/item*.xml` datastore part is authored + linked;
///   - the wrapped run text (the opaque/content inventory) is non-shrinking.
#[test]
fn wrap_with_data_binding_is_faithful() {
    let bytes = make_test_docx(&["The Counterparty shall sign the agreement."]);
    let parsed = Document::parse(&bytes).expect("parse");
    let base = parsed.snapshot().canonical.clone();
    let block_id = match &base.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("expected a paragraph"),
    };
    let store_id = "{ABCDEF01-2345-6789-ABCD-EF0123456789}";
    let mk = || EditStep::WrapInContentControl {
        block_id: block_id.clone(),
        expect: "Counterparty".to_string(),
        semantic_hash: None,
        spec: SdtSpec {
            tag: Some("party".to_string()),
            alias: Some("Counterparty".to_string()),
            control: SdtControl::PlainText,
            binding: Some(DataBinding {
                xpath: "/ns0:contract[1]/ns0:party[1]".to_string(),
                store_item_id: store_id.to_string(),
                prefix_mappings: Some("xmlns:ns0='urn:contract'".to_string()),
            }),
        },
        rationale: None,
    };

    // Untracked: tracked-mode and direct-mode produce identical IR.
    let tracked = apply_transaction(&base, &txn(vec![mk()], MaterializationMode::TrackedChange))
        .expect("tracked apply")
        .0;
    let direct = apply_transaction(&base, &txn(vec![mk()], MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        shape(&tracked),
        shape(&direct),
        "untracked data-bound wrap: tracked == direct shape"
    );

    // On the serialized markup, accept-all == reject-all == the bound doc.
    let edited_doc = parsed
        .apply(&txn(vec![mk()], MaterializationMode::Direct))
        .expect("apply");
    let ser =
        |d: &Document| -> Vec<u8> { d.serialize(&ExportOptions::default()).expect("serialize") };
    let read_doc_xml = |b: &[u8]| -> String {
        let a = stemma::docx::DocxArchive::read(b).expect("read");
        String::from_utf8(a.get("word/document.xml").expect("doc.xml").to_vec()).expect("utf8")
    };
    let bound_bytes = ser(&edited_doc);
    let accept_bytes = ser(&edited_doc.project(Resolution::AcceptAll).expect("accept"));
    let reject_bytes = ser(&edited_doc.project(Resolution::RejectAll).expect("reject"));
    let bound_xml = read_doc_xml(&bound_bytes);
    assert_eq!(
        read_doc_xml(&accept_bytes),
        bound_xml,
        "serialized accept-all == bound"
    );
    assert_eq!(
        read_doc_xml(&reject_bytes),
        bound_xml,
        "serialized reject-all == bound"
    );
    for (label, xml) in [
        ("bound", &bound_xml),
        ("accept", &read_doc_xml(&accept_bytes)),
        ("reject", &read_doc_xml(&reject_bytes)),
    ] {
        assert!(
            xml.contains("<w:sdt") && xml.contains("<w:dataBinding "),
            "[{label}] sdt + dataBinding survive"
        );
        assert!(
            !xml.contains("w:sdtChange"),
            "[{label}] no w:sdtChange envelope"
        );
        assert!(
            !xml.contains("<w:ins") && !xml.contains("<w:del"),
            "[{label}] not a tracked change"
        );
    }

    // The backing datastore part is authored, content-typed, and linked.
    let archive = stemma::docx::DocxArchive::read(&bound_bytes).expect("read out");
    let names: Vec<String> = archive.list().map(|s| s.to_string()).collect();
    assert!(
        names.iter().any(|n| n.starts_with("customXml/item")
            && n.ends_with(".xml")
            && !n.contains("itemProps")
            && !n.contains("_rels")),
        "a customXml datastore part must be authored; parts={names:?}"
    );
    let doc_rels = String::from_utf8(
        archive
            .get("word/_rels/document.xml.rels")
            .expect("rels")
            .to_vec(),
    )
    .unwrap();
    assert!(
        doc_rels.contains("relationships/customXml"),
        "the datastore must be linked from document.xml"
    );

    // Non-shrinking content: the wrapped text survives inside the bound doc.
    assert!(
        bound_xml.contains("Counterparty"),
        "wrapped text must survive the data-bound wrap"
    );
}

/// `InsertImage` is a TRACKED insert: the synthesized drawing rides in its own
/// `Inserted` segment, so the three standing invariants must hold —
/// reject-all reconstructs the baseline, accept-all equals direct, and the
/// opaque inventory does not shrink. The generic harness covers all three; the
/// `shape()` fingerprint surfaces the inserted opaque anchor.
#[test]
fn insert_image_is_faithful() {
    let (base, ids) = doc_and_ids(&["A paragraph to host an image."]);
    let png = {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&[0xABu8; 16]);
        v
    };
    let image = ImageSource::new(
        png,
        ImageFormat::Png,
        914_400,
        457_200,
        Some("art".into()),
        0,
    )
    .expect("valid png source");
    assert_fidelity(
        "insert_image",
        &base,
        vec![EditStep::InsertImage {
            block_id: NodeId::from(ids[0].as_str()),
            expect: None,
            semantic_hash: None,
            image,
            rationale: None,
        }],
    );
}

// ─── Read-surface projection (comprehension / roadmap A) ───────────────────────

/// A DOCX whose single paragraph carries a `<w:fldSimple>` opaque anchor between
/// two text runs, so the plain-text projection's one-U+FFFC-per-opaque invariant
/// has an anchor to count. `make_test_docx` has no opaque inlines.
fn make_field_docx() -> Vec<u8> {
    let body = r#"<w:p><w:r><w:t>See </w:t></w:r><w:fldSimple w:instr=" REF Defs \h "><w:r><w:t>Section 2</w:t></w:r></w:fldSimple><w:r><w:t> for details</w:t></w:r></w:p>"#;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
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

/// The read surface (`to_plain_text`) is a faithful, opaque-preserving projection
/// of the materialized state — the read-only analogue of the per-verb fidelity
/// gate:
///
/// 1. **Reversibility at the text layer.** A redline's reject-all plain text
///    reconstructs the base's plain text; accept-all reconstructs the target's.
///    (No verb to reverse here — the projection itself must agree with the
///    engine's accept/reject.)
/// 2. **Accept equals direct.** accept-all plain text == the target document's
///    own plain text (the target is the direct/untracked truth).
/// 3. **Non-shrinking opaque inventory through the identity surface.** the
///    block-identity text (`extract_block_text`) emits exactly one U+FFFC per
///    opaque anchor in the IR, and that count does not shrink across the clean
///    accept-all projection. (This is the hash/identity contract — *not* the
///    human-readable `to_plain_text`, which surfaces a field's cached result as
///    text; the two surfaces are deliberately distinct, see `to_plain_text`.)
#[test]
fn read_surface_plain_text_is_faithful() {
    use stemma::Resolution;
    use stemma::import::extract_block_text;
    use stemma::view::to_plain_text;

    // (1)/(2) text-layer reversibility + accept==direct via diff.
    let base = Document::parse(&make_test_docx(&["Hello world"])).expect("base");
    let target = Document::parse(&make_test_docx(&["Hello brave world"])).expect("target");
    let redlined = base.diff(&target).expect("diff");

    let rejected = redlined.project(Resolution::RejectAll).expect("reject");
    let accepted = redlined.project(Resolution::AcceptAll).expect("accept");
    assert_eq!(
        to_plain_text(&rejected.read()),
        to_plain_text(&base.read()),
        "reject-all plain text reconstructs the base"
    );
    assert_eq!(
        to_plain_text(&accepted.read()),
        to_plain_text(&target.read()),
        "accept-all plain text == target plain text (accept == direct)"
    );

    // (3) one U+FFFC per opaque anchor on the IDENTITY surface, non-shrinking
    // across the clean accept-all projection. The fixture's fldSimple carries a
    // cached result ("Section 2").
    let field_doc = Document::parse(&make_field_docx()).expect("field doc");
    let inventory_before = anchor_ids(&field_doc.snapshot().canonical).len();
    assert_eq!(
        inventory_before, 1,
        "fixture carries exactly one opaque anchor"
    );

    // Identity surface: one U+FFFC per anchor, stable against the field result.
    let identity_fffc = |doc: &Document| -> usize {
        doc.snapshot()
            .canonical
            .blocks
            .iter()
            .map(|tb| extract_block_text(&tb.block).matches('\u{FFFC}').count())
            .sum()
    };
    assert_eq!(
        identity_fffc(&field_doc),
        inventory_before,
        "identity surface emits exactly one U+FFFC per opaque anchor"
    );
    // Human-readable surface: the field reads as its cached result, not a U+FFFC.
    assert_eq!(
        to_plain_text(&field_doc.read()),
        "See Section 2 for details",
        "human-readable surface shows the field's cached result"
    );

    let field_accepted = field_doc
        .project(Resolution::AcceptAll)
        .expect("accept field doc");
    let inventory_after = anchor_ids(&field_accepted.snapshot().canonical).len();
    assert!(
        inventory_after >= inventory_before,
        "opaque inventory must not shrink across accept-all projection"
    );
    assert_eq!(
        identity_fffc(&field_accepted),
        inventory_after,
        "accept-all identity surface still emits one U+FFFC per surviving opaque anchor"
    );
}
// ─── Agentic MCP surface (roadmap E): selective resolution fidelity ───────────
//
// The MCP `accept_changes` / `reject_changes` tools lower a selector to a
// `HashSet<u32>` of revision ids and call `Resolution::Selective{ids, action}`
// (via `SimpleRuntime::resolve_tracked_revisions`). This is not a new engine
// verb — it is the existing selective-projection path made reachable over
// stdio. The fidelity contract it must satisfy, beyond the per-verb gate above:
//
//   * SELECTIVE reject of the FULL change set == reject-all == the original
//     (reversibility through the selective path);
//   * SELECTIVE accept of a partition (set S) THEN selective accept of the rest
//     == accept-all == direct apply (the partition composes);
//   * the opaque inventory never shrinks under selective resolution.
//
// We build a document carrying two independently-authored tracked changes
// (Alice on block 0, Bob on block 1) and drive the selective path exactly as
// the tool does, asserting on `shape()` (engine-independent content fingerprint).

/// Collect the revision ids attributed to `author` from a tracked document's
/// read view — the lowering the MCP `ByAuthor` selector performs.
fn revision_ids_by_author(canon: &CanonDoc, author: &str) -> std::collections::HashSet<u32> {
    let view = build_document_view_from_canon(canon);
    let mut ids = std::collections::HashSet::new();
    let mut push = |s: &TrackStatus| {
        if let TrackStatus::Inserted(r) | TrackStatus::Deleted(r) = s
            && r.author.as_deref() == Some(author)
        {
            ids.insert(r.revision_id);
        }
    };
    for b in &view.blocks {
        push(&b.block_status);
        push(&b.paragraph_mark_status);
        for seg in &b.segments {
            match seg {
                SegmentView::Text { status, .. } | SegmentView::Opaque { status, .. } => {
                    push(status)
                }
            }
        }
    }
    ids
}

fn authored_txn(steps: Vec<EditStep>, revision_id: u32, author: &str) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id,
            author: Some(author.to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

#[test]
fn selective_resolution_is_faithful() {
    let base = Document::parse(&make_test_docx(&[
        "First clause text",
        "Second clause text",
    ]))
    .expect("parse base");
    let ids: Vec<String> = base
        .read()
        .blocks
        .iter()
        .map(|b| b.id.to_string())
        .collect();
    let base_canon = base.snapshot().canonical.clone();

    // Two independently-authored tracked changes.
    let alice = authored_txn(
        vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(ids[0].as_str()),
            rationale: None,
            replacement_role: None,
            expect: "First clause text".to_string(),
            semantic_hash: None,
            content: text_content("First clause AMENDED"),
        }],
        1,
        "Alice",
    );
    let bob = authored_txn(
        vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(ids[1].as_str()),
            rationale: None,
            replacement_role: None,
            expect: "Second clause text".to_string(),
            semantic_hash: None,
            content: text_content("Second clause AMENDED"),
        }],
        2,
        "Bob",
    );
    let two = base
        .apply(&alice)
        .expect("apply alice")
        .apply(&bob)
        .expect("apply bob");
    let two_canon = (*two.snapshot().canonical).clone();

    // Lower the selectors against the live read view (the tool's surface).
    let alice_ids = revision_ids_by_author(&two_canon, "Alice");
    let bob_ids = revision_ids_by_author(&two_canon, "Bob");
    assert!(
        !alice_ids.is_empty() && !bob_ids.is_empty(),
        "both authors have ids"
    );
    assert!(
        alice_ids.is_disjoint(&bob_ids),
        "author id sets are disjoint"
    );
    let all_ids: std::collections::HashSet<u32> = &alice_ids | &bob_ids;

    // 1. Reversibility: SELECTIVE reject of the full change set == the original.
    let sel_reject_all = two
        .project(Resolution::Selective {
            ids: all_ids.clone(),
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject all");
    assert_eq!(
        shape(&base_canon),
        shape(&sel_reject_all.snapshot().canonical),
        "selective reject of the full set must reconstruct the original"
    );

    // 2. Partition composes: selective accept Alice, then selective accept Bob,
    //    equals accept-all equals direct apply of both edits in order.
    let accept_alice = two
        .project(Resolution::Selective {
            ids: alice_ids.clone(),
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept alice");
    // After accepting Alice, Bob's ids must still be resolvable (still tracked).
    let bob_ids_after = revision_ids_by_author(&accept_alice.snapshot().canonical, "Bob");
    assert!(
        !bob_ids_after.is_empty(),
        "Bob's change must remain tracked after accepting Alice"
    );
    let accept_both = accept_alice
        .project(Resolution::Selective {
            ids: bob_ids_after,
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept bob");

    let mut accept_all_canon = two_canon.clone();
    accept_all(&mut accept_all_canon);
    assert_eq!(
        shape(&accept_both.snapshot().canonical),
        shape(&accept_all_canon),
        "selective accept of a partition must equal accept-all"
    );

    let direct1 = apply_transaction(
        &base_canon,
        &EditTransaction {
            materialization_mode: MaterializationMode::Direct,
            ..authored_txn(alice.steps.clone(), 1, "Alice")
        },
    )
    .expect("direct alice")
    .0;
    let direct2 = apply_transaction(
        &direct1,
        &EditTransaction {
            materialization_mode: MaterializationMode::Direct,
            ..authored_txn(bob.steps.clone(), 2, "Bob")
        },
    )
    .expect("direct bob")
    .0;
    assert_eq!(
        shape(&accept_both.snapshot().canonical),
        shape(&direct2),
        "selective accept of a partition must equal direct apply of both edits"
    );

    // 3. Non-shrinking opaque inventory under selective resolution.
    let before = anchor_ids(&base_canon);
    for id in &before {
        assert!(
            anchor_ids(&accept_both.snapshot().canonical).contains(id),
            "opaque anchor '{id}' dropped under selective accept"
        );
    }
}

// ─── CommentCreate: an annotation, NOT a tracked change ──────────────────────
//
// CommentCreate is special: it is an annotation, so the generic `assert_fidelity`
// harness does NOT apply (reject-all must NOT remove the comment — comments are
// not w:ins/w:del). The three comment-anchor markers DO now surface in the
// standing `shape()` fingerprint (as `SegmentView::Opaque` entries of kind
// `Comment`/`CommentRangeStart`/`CommentRangeEnd`) — but `shape()` alone can't
// distinguish "reject-all correctly left an annotation in place" from "reject-all
// failed to remove tracked content", so we still assert directly on the IR here:
// (1) the opaque inventory is non-shrinking; (2) the three anchor markers and the
// comment story survive BOTH accept-all and reject-all. (The fuzz-transaction
// sweep's generic reversibility check, `fuzz_transactions.rs`'s I1, strips these
// same comment-anchor lines before comparing — see `shape_for_reversibility`
// there — so a fuzzed `CommentCreate` step doesn't spuriously fail it.)

/// Count (rangeStart, rangeEnd, reference) markers for `id` across body
/// paragraphs, counting the reference in either its zero-width or opaque form.
fn comment_marker_counts(doc: &CanonDoc, id: &str) -> (usize, usize, usize) {
    let (mut s, mut e, mut r) = (0, 0, 0);
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    match inline {
                        InlineNode::CommentRangeStart { id: i } if i == id => s += 1,
                        InlineNode::CommentRangeEnd { id: i } if i == id => e += 1,
                        InlineNode::CommentReference { id: i } if i == id => r += 1,
                        InlineNode::OpaqueInline(o) => {
                            if let OpaqueKind::CommentReference(rd) = &o.kind
                                && rd.reference_id == id
                            {
                                r += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    (s, e, r)
}

#[test]
fn comment_create_is_an_annotation_surviving_accept_and_reject() {
    let (base, ids) = doc_and_ids(&["The Confidential Information is protected."]);
    let block_id = NodeId::from(ids[0].as_str());

    let edited = apply_transaction(
        &base,
        &txn(
            vec![EditStep::CommentCreate {
                block_id,
                expect: "Confidential Information".to_string(),
                semantic_hash: None,
                body: "Verify the definition cross-reference.".to_string(),
                author: Some("Gate".to_string()),
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("comment create")
    .0;

    assert_eq!(edited.comments.len(), 1, "comment story authored");
    let cid = edited.comments[0].id.clone();
    assert_eq!(
        comment_marker_counts(&edited, &cid),
        (1, 1, 1),
        "one of each anchor marker present"
    );

    // Non-shrinking opaque inventory: every pre-edit opaque anchor survives.
    let before = anchor_ids(&base);
    let after = anchor_ids(&edited);
    for id in &before {
        assert!(
            after.contains(id),
            "opaque anchor '{id}' dropped by CommentCreate"
        );
    }

    // Annotation invariant: BOTH accept-all and reject-all retain the story and
    // all three markers (comments are not w:ins/w:del, so neither resolution
    // removes them).
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(accepted.comments.len(), 1, "accept-all keeps comment story");
    assert_eq!(
        comment_marker_counts(&accepted, &cid),
        (1, 1, 1),
        "accept-all keeps all anchor markers"
    );

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(rejected.comments.len(), 1, "reject-all keeps comment story");
    assert_eq!(
        comment_marker_counts(&rejected, &cid),
        (1, 1, 1),
        "reject-all keeps all anchor markers"
    );

    // The body's visible text is unchanged by the annotation under both
    // resolutions (the comment range wraps text, it does not alter it).
    assert_eq!(
        shape(&accepted),
        shape(&rejected),
        "comment annotation leaves visible body content identical under accept and reject"
    );
}

// ─── InsertNote / DeleteNote: footnote reference is a tracked body insert ─────
//
// The footnote/endnote REFERENCE run is an inline opaque spliced into the body
// as a tracked insert — so the generic `assert_fidelity` harness applies to it
// directly: reject-all drops the reference (== baseline), accept-all == direct,
// and the PRE-EXISTING opaque inventory (image + field) must not shrink. The
// note STORY lives outside the body fingerprint, so we assert its lifecycle
// (created on insert, present on accept) directly on the IR.

/// A single paragraph that already hosts an inline image (`w:drawing`) AND a
/// `w:fldSimple` field, with anchor text between them. The non-shrinking opaque
/// inventory check then has real pre-existing opaques to protect.
fn make_image_and_field_docx() -> Vec<u8> {
    let drawing = r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><wp:extent cx="100" cy="200"/><wp:docPr id="1" name="Picture 1" descr="alt"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:ext cx="9" cy="8"/></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r>{drawing}</w:r><w:r><w:t xml:space="preserve">See the Definitions clause for details.</w:t></w:r><w:fldSimple w:instr="REF Definitions \h"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p><w:sectPr/></w:body></w:document>"#
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

#[test]
fn insert_note_is_faithful_over_image_and_field_paragraph() {
    let base = Document::parse(&make_image_and_field_docx())
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let block_id = match &base.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        other => panic!("expected paragraph, got {other:?}"),
    };
    // Baseline already carries two opaques (the drawing + the field).
    assert_eq!(
        anchor_ids(&base).len(),
        2,
        "fixture must carry an image + a field opaque"
    );

    let steps = vec![EditStep::InsertNote {
        block_id,
        expect: "Definitions".to_string(),
        semantic_hash: None,
        note_kind: NoteKind::Footnote,
        body: "See Schedule 2.".to_string(),
        rationale: None,
    }];

    // Generic gate: reversibility (the reference run disappears on reject),
    // accept==direct, non-shrinking opaque inventory (image + field survive).
    assert_fidelity("insert_footnote", &base, steps.clone());

    // Story lifecycle (outside the body fingerprint): created on insert, present
    // after accept-all.
    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    let normal_ids: Vec<_> = tracked
        .footnotes
        .iter()
        .filter(|f| matches!(f.note_type, NoteType::Normal))
        .map(|f| f.id.clone())
        .collect();
    assert_eq!(normal_ids.len(), 1, "one footnote story authored");

    let mut accepted = tracked;
    accept_all(&mut accepted);
    assert_eq!(
        accepted
            .footnotes
            .iter()
            .filter(|f| matches!(f.note_type, NoteType::Normal))
            .count(),
        1,
        "accept-all keeps the footnote story"
    );

    // accept-all == direct on the story body too.
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    let body_of = |c: &CanonDoc| -> String {
        c.footnotes
            .iter()
            .find(|f| matches!(f.note_type, NoteType::Normal))
            .map(|f| match &f.blocks[0].block {
                BlockNode::Paragraph(p) => p
                    .segments
                    .iter()
                    .flat_map(|s| s.inlines.iter())
                    .filter_map(|i| match i {
                        InlineNode::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<String>(),
                _ => String::new(),
            })
            .unwrap_or_default()
    };
    assert_eq!(
        body_of(&accepted),
        body_of(&direct),
        "story body equals direct"
    );
}

/// DeleteNote round-trip: inserting a footnote (direct) then deleting it returns
/// the body to its original shape and leaves no footnote story — and the
/// pre-existing image + field opaques are untouched throughout.
#[test]
fn delete_note_returns_to_baseline() {
    let base = Document::parse(&make_image_and_field_docx())
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let block_id = match &base.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        other => panic!("expected paragraph, got {other:?}"),
    };

    // Visible body text of the first paragraph (run re-segmentation with
    // identical formatting is semantically irrelevant in OOXML — see this
    // file's `shape()` doc comment — so the reversibility contract for an
    // anchor-splice insert/delete is visible-text identity, not byte-identical
    // run boundaries).
    fn body_text(canon: &CanonDoc) -> String {
        match &canon.blocks[0].block {
            BlockNode::Paragraph(p) => p
                .segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .filter_map(|i| match i {
                    InlineNode::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect(),
            _ => panic!("expected paragraph"),
        }
    }
    let base_text = body_text(&base);

    // Insert (direct) then delete the note.
    let inserted = apply_transaction(
        &base,
        &txn(
            vec![EditStep::InsertNote {
                block_id,
                expect: "Definitions".to_string(),
                semantic_hash: None,
                note_kind: NoteKind::Footnote,
                body: "Temp note.".to_string(),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("insert")
    .0;
    let note_id = inserted
        .footnotes
        .iter()
        .find(|f| matches!(f.note_type, NoteType::Normal))
        .expect("story authored")
        .id
        .clone();

    let deleted = apply_transaction(
        &inserted,
        &txn(
            vec![EditStep::DeleteNote {
                note_id: note_id.clone(),
                note_kind: NoteKind::Footnote,
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("delete")
    .0;

    // Body visible text returns to baseline; no story remains; image + field
    // survive.
    assert_eq!(
        body_text(&deleted),
        base_text,
        "insert-then-delete returns the body to its original visible text"
    );
    assert!(
        !deleted.footnotes.iter().any(|f| f.id == note_id),
        "no footnote story remains after delete"
    );
    assert_eq!(
        anchor_ids(&deleted).len(),
        2,
        "the pre-existing image + field opaques are untouched by note insert/delete"
    );
}

// ─── SetPageSetup ────────────────────────────────────────────────────────────

/// SetPageSetup is a section-property delta (not a text edit), so the generic
/// `shape()` fingerprint is blind to it (it captures block/segment content, not
/// `w:sectPr`). We therefore (a) run the generic harness to prove the edit
/// leaves body CONTENT untouched and the opaque inventory non-shrinking, and
/// (b) assert reversibility / accept==direct DIRECTLY on the IR section
/// properties, which is the only place the change is observable.
#[test]
fn set_page_setup_is_faithful() {
    let (base, _ids) = doc_and_ids(&["Body paragraph one.", "Body paragraph two."]);
    // The fixture's `<w:sectPr/>` parses into Some(default); confirm the base
    // section is portrait-ish (no orientation set) so the flip is observable.
    assert!(base.body_section_properties.is_some());

    let steps = vec![EditStep::SetPageSetup {
        target: SectionTarget::Body,
        patch: PageSetupPatch {
            orientation: Some(PageOrientation::Landscape),
            margins: Some(PageMargins {
                top: 720,
                bottom: 720,
                left: 1440,
                right: 1440,
                header: 360,
                footer: 360,
            }),
            ..Default::default()
        },
        semantic_hash: None,
        rationale: None,
    }];

    // (a) Generic gate: body content + opaque inventory unchanged by a sectPr delta.
    assert_fidelity("set_page_setup", &base, steps.clone());

    // (b) Direct IR assertions on the section properties.
    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    // TrackedChange records the prior sectPr as a sectPrChange.
    assert!(
        tracked.body_section_property_change.is_some(),
        "tracked SetPageSetup must record a w:sectPrChange"
    );
    assert_eq!(
        tracked
            .body_section_properties
            .as_ref()
            .unwrap()
            .orientation,
        Some(PageOrientation::Landscape),
        "the new section is landscape"
    );

    // Reject-all restores the PRIOR section properties (orientation back to base).
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        rejected
            .body_section_properties
            .as_ref()
            .unwrap()
            .orientation,
        base.body_section_properties.as_ref().unwrap().orientation,
        "reject-all restores the prior orientation"
    );
    assert!(
        rejected.body_section_property_change.is_none(),
        "reject-all clears the sectPrChange"
    );

    // Accept-all equals Direct apply on the section properties.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        accepted
            .body_section_properties
            .as_ref()
            .unwrap()
            .orientation,
        direct.body_section_properties.as_ref().unwrap().orientation,
        "accept-all orientation equals direct"
    );
    assert_eq!(
        accepted
            .body_section_properties
            .as_ref()
            .unwrap()
            .margin_left,
        direct.body_section_properties.as_ref().unwrap().margin_left,
        "accept-all margins equal direct"
    );
    assert!(
        accepted.body_section_property_change.is_none(),
        "accept-all clears the sectPrChange"
    );
}

// ─── InsertEquation (Verb A) ─────────────────────────────────────────────────

const EQ_M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

fn omath_inline_fragment() -> Vec<u8> {
    format!(r#"<m:oMath xmlns:m="{EQ_M_NS}"><m:r><m:t>a+b</m:t></m:r></m:oMath>"#).into_bytes()
}

fn omath_para_fragment() -> Vec<u8> {
    format!(
        r#"<m:oMathPara xmlns:m="{EQ_M_NS}"><m:oMath><m:r><m:t>y=x</m:t></m:r></m:oMath></m:oMathPara>"#
    )
    .into_bytes()
}

/// InsertEquation is a tracked insert of an OMML opaque: it must satisfy the
/// three fidelity invariants (reject==baseline, accept==direct, non-shrinking
/// opaque inventory) for both inline and block placements.
#[test]
fn insert_equation_inline_is_faithful() {
    let (base, ids) = doc_and_ids(&["The value of x matters here"]);
    assert_fidelity(
        "insert_equation_inline",
        &base,
        vec![EditStep::InsertEquation {
            block_id: NodeId::from(ids[0].as_str()),
            expect: "value".to_string(),
            semantic_hash: None,
            omml: omath_inline_fragment(),
            placement: EquationPlacement::Inline,
            rationale: None,
        }],
    );
}

#[test]
fn insert_equation_block_is_faithful() {
    let (base, ids) = doc_and_ids(&["Display the equation below please"]);
    assert_fidelity(
        "insert_equation_block",
        &base,
        vec![EditStep::InsertEquation {
            block_id: NodeId::from(ids[0].as_str()),
            expect: "below".to_string(),
            semantic_hash: None,
            omml: omath_para_fragment(),
            placement: EquationPlacement::Block,
            rationale: None,
        }],
    );
}

// ─── Granular table ops (table_ops) ──────────────────────────────────────────
//
// The three fidelity invariants apply to structural table edits too: reject-all
// restores the base table, accept-all equals direct-mode apply, and no opaque
// anchor is dropped. These route through the same table-diff machinery as
// `replace(table)`.

fn table_fidelity_para(id: &str, text: &str) -> ParagraphNode {
    ParagraphNode {
        id: NodeId::from(id),
        style_id: None,
        align: None,
        has_direct_align: false,
        indent: None,
        has_direct_indent: false,
        authored_indent: None,
        spacing: None,
        has_direct_spacing: false,
        authored_spacing: None,
        borders: None,
        keep_next: None,
        keep_lines: None,
        page_break_before: false,
        widow_control: None,
        contextual_spacing: None,
        shading: None,
        has_direct_keep_next: true,
        has_direct_keep_lines: true,
        has_direct_page_break_before: true,
        has_direct_widow_control: true,
        has_direct_contextual_spacing: true,
        has_direct_shading: true,
        has_direct_borders: true,
        tab_stops: vec![],
        effective_tab_stops_rel: vec![],
        segments: normal_segment(vec![InlineNode::from(TextNode {
            id: NodeId::from(format!("{id}_t")),
            text_role: None,
            text: text.to_string(),
            marks: vec![],
            style_props: StyleProps::default(),
            rpr_authored: stemma::domain::RunRprAuthored::default(),
            formatting_change: None,
        })]),
        block_text_hash: None,
        numbering: None,
        has_direct_numbering: true,
        numbering_suppressed: false,
        materialized_numbering: None,
        rendered_text: None,
        literal_prefix: None,
        literal_prefix_leading_rpr: None,
        literal_prefix_trailing_rpr: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: stemma::domain::RunRprAuthored::default(),
        literal_prefix_leading_tab_twips: None,
        literal_prefix_leading_tab_count: 0,
        literal_prefix_leading_ws: String::new(),
        literal_prefix_trailing_ws: String::new(),
        literal_prefix_has_trailing_tab: false,
        literal_prefix_trailing_tab_stop_twips: None,
        outline_lvl: None,
        heading_level: None,
        para_mark_status: None,
        paragraph_mark_marks: vec![],
        paragraph_mark_style_props: StyleProps::default(),
        paragraph_mark_rpr_off: Default::default(),
        para_split: false,
        section_property_change: None,
        formatting_change: None,
        section_properties: None,
        mirror_indents: None,
        auto_space_de: None,
        auto_space_dn: None,
        bidi: None,
        text_alignment: None,
        suppress_auto_hyphens: None,
        snap_to_grid: None,
        overflow_punct: None,
        adjust_right_ind: None,
        word_wrap: None,
        frame_pr: None,
        para_id: None,
        text_id: None,
        text_direction: None,
        cnf_style: None,
        preserved_ppr: Vec::new(),
    }
}

fn table_fidelity_cell(id: &str, text: &str) -> TableCellNode {
    TableCellNode {
        id: NodeId::from(id),
        blocks: vec![BlockNode::from(table_fidelity_para(
            &format!("{id}_p"),
            text,
        ))],
        grid_span: 1,
        v_merge: VerticalMerge::None,
        formatting: CellFormatting::default(),
        formatting_change: None,
        tracking_status: None,
        row_sdt_wrapper: None,
        content_sdt_wraps: Vec::new(),
        cnf_style: None,
        hide_mark: false,
        preserved: Vec::new(),
    }
}

fn table_fidelity_row(id: &str, cells: Vec<TableCellNode>) -> TableRowNode {
    TableRowNode {
        id: NodeId::from(id),
        cells,
        grid_before: 0,
        grid_after: 0,
        tracking_status: None,
        is_header: false,
        height: None,
        height_rule: None,
        formatting_change: None,
        para_id: None,
        text_id: None,
        cant_split: false,
        jc: None,
        w_before: None,
        w_after: None,
        cnf_style: None,
        tbl_pr_ex: None,
        cell_spacing: None,
        preserved: Vec::new(),
    }
}

fn table_fidelity_doc() -> CanonDoc {
    let table = TableNode {
        id: NodeId::from("tf_t"),
        rows: vec![
            table_fidelity_row(
                "tf_r0",
                vec![
                    table_fidelity_cell("tf_r0c0", "A"),
                    table_fidelity_cell("tf_r0c1", "B"),
                ],
            ),
            table_fidelity_row(
                "tf_r1",
                vec![
                    table_fidelity_cell("tf_r1c0", "C"),
                    table_fidelity_cell("tf_r1c1", "D"),
                ],
            ),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    let mut doc = doc_and_ids(&["body"]).0;
    doc.blocks
        .push(normal_tracked_block(BlockNode::from(table)));
    doc
}

fn table_step(op: TableOp) -> EditStep {
    EditStep::TableStructureOp {
        block_id: NodeId::from("tf_t"),
        semantic_hash: None,
        op,
        rationale: None,
    }
}

#[test]
fn table_op_insert_row_is_faithful() {
    assert_fidelity(
        "table_op_insert_row",
        &table_fidelity_doc(),
        vec![table_step(TableOp::InsertRow {
            ref_row: 1,
            position: TableInsertPosition::After,
            cells: None,
        })],
    );
}

#[test]
fn table_op_delete_row_is_faithful() {
    assert_fidelity(
        "table_op_delete_row",
        &table_fidelity_doc(),
        vec![table_step(TableOp::DeleteRow { row_index: 0 })],
    );
}

#[test]
fn table_op_insert_column_is_faithful() {
    assert_fidelity(
        "table_op_insert_column",
        &table_fidelity_doc(),
        vec![table_step(TableOp::InsertColumn {
            ref_col: 1,
            position: TableInsertPosition::After,
        })],
    );
}

#[test]
fn table_op_delete_column_is_faithful() {
    assert_fidelity(
        "table_op_delete_column",
        &table_fidelity_doc(),
        vec![table_step(TableOp::DeleteColumn { col_index: 0 })],
    );
}

#[test]
fn table_op_merge_cells_is_faithful() {
    assert_fidelity(
        "table_op_merge_cells",
        &table_fidelity_doc(),
        vec![table_step(TableOp::MergeCells {
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 1,
        })],
    );
}

// ─── Table-level formatting (SetTableFormatting → w:tblPrChange) ───────────────
//
// An in-place `tblPr` property delta (borders / width / default cell margins).
// The generic `shape()` fingerprint is BLIND to tblPr (it captures text/merge
// structure, not table borders), so a broken reversal would pass it. We
// therefore (a) run the generic harness — it proves the rows/cells are
// untouched and the opaque inventory is non-shrinking — and (b) assert
// reversibility/accept==direct DIRECTLY on the IR `TableFormatting`, the only
// place these values are observable.

/// A table-fidelity doc whose table carries explicit borders + width (so the
/// snapshot/reject path has a real prior state to restore).
fn formatted_table_fidelity_doc() -> CanonDoc {
    let edge = Border {
        style: BorderStyle::Single,
        color: Some("000000".to_string()),
        size: Some(4),
        space: Some(0),
        extra_attrs: Vec::new(),
    };
    let single = BorderSet {
        top: Some(edge.clone()),
        bottom: Some(edge.clone()),
        left: Some(edge.clone()),
        right: Some(edge.clone()),
        inside_h: Some(edge.clone()),
        inside_v: Some(edge),
    };
    let table = TableNode {
        id: NodeId::from("tf_t"),
        rows: vec![
            table_fidelity_row(
                "tf_r0",
                vec![
                    table_fidelity_cell("tf_r0c0", "A"),
                    table_fidelity_cell("tf_r0c1", "B"),
                ],
            ),
            table_fidelity_row(
                "tf_r1",
                vec![
                    table_fidelity_cell("tf_r1c0", "C"),
                    table_fidelity_cell("tf_r1c1", "D"),
                ],
            ),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting {
            borders: Some(single),
            width: Some(TableMeasurement {
                w: 5000,
                width_type: WidthType::Pct,
                pct_literal: None,
            }),
            ..TableFormatting::default()
        },
        formatting_change: None,
    };
    let mut doc = doc_and_ids(&["body"]).0;
    doc.blocks
        .push(normal_tracked_block(BlockNode::from(table)));
    doc
}

fn fidelity_table(doc: &CanonDoc) -> &TableNode {
    doc.blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) => Some(t),
            _ => None,
        })
        .expect("table block present")
}

#[test]
fn set_table_formatting_is_faithful() {
    let base = formatted_table_fidelity_doc();
    let double = BorderSet {
        top: Some(Border {
            style: BorderStyle::Double,
            color: Some("FF0000".to_string()),
            size: Some(12),
            space: Some(0),
            extra_attrs: Vec::new(),
        }),
        ..Default::default()
    };
    let margins = CellMargins {
        top: Some(60),
        bottom: Some(60),
        left: Some(120),
        right: Some(120),
    };
    let steps = vec![EditStep::SetTableFormatting {
        block_id: NodeId::from("tf_t"),
        semantic_hash: None,
        patch: TableFormattingPatch {
            borders: Some(double.clone()),
            width: None,
            default_cell_margins: Some(margins.clone()),
        },
        rationale: None,
    }];

    // (a) Generic structural gate: rows/cells reverse, opaque inventory holds.
    assert_fidelity("set_table_formatting", &base, steps.clone());

    // (b) Direct IR assertions on the tblPr that shape() cannot see.
    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    // Reject-all restores the original tblPr exactly.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        fidelity_table(&rejected).formatting,
        fidelity_table(&base).formatting,
        "reject-all must restore the original tblPr (borders + width + margins)"
    );
    // Accept-all equals direct apply on the tblPr.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        fidelity_table(&accepted).formatting,
        fidelity_table(&direct).formatting,
        "accept-all tblPr must equal direct-apply tblPr"
    );
    // The new state carries the requested fields; the untouched width survives.
    let dt = fidelity_table(&direct);
    assert_eq!(dt.formatting.borders, Some(double));
    assert_eq!(dt.formatting.default_cell_margins, Some(margins));
    assert_eq!(
        dt.formatting.width,
        Some(TableMeasurement {
            w: 5000,
            width_type: WidthType::Pct,
            pct_literal: None,
        }),
        "the patch left width untouched"
    );
}

// ─── create-header / create-footer (net-new story + tracked sectPrChange) ─────
//
// `CreateHeader`/`CreateFooter` author a blank story PLUS a tracked
// `w:sectPrChange` that adds its body-section reference. The standing `shape()`
// fingerprint is BLIND to header/footer stories and section properties, so a
// broken reversal would still pass the generic gate. We therefore (a) run the
// generic harness (it proves the body content reverses and the opaque inventory
// is non-shrinking) and (b) assert reversibility / accept==direct DIRECTLY on
// the IR `doc.headers` / `doc.footers` + `body_section_properties.{header,footer}_refs`,
// the only place this verb's effect is observable.

/// The (kind, part_path) of every header reference on the body section, sorted.
fn body_header_refs(canon: &CanonDoc) -> Vec<(HeaderFooterKind, String)> {
    let mut v: Vec<(HeaderFooterKind, String)> = canon
        .body_section_properties
        .as_ref()
        .map(|sp| {
            sp.header_refs
                .iter()
                .map(|r| (r.kind.clone(), r.part_path.clone()))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
}

/// The part names of every header story, sorted.
fn header_part_names(canon: &CanonDoc) -> Vec<String> {
    let mut v: Vec<String> = canon.headers.iter().map(|h| h.part_name.clone()).collect();
    v.sort();
    v
}

/// `CreateHeader` authors a NET-NEW `Even` header. (The importer always
/// materializes a blank `Default` header reference per §17.10.2, so `Even` — and
/// `First` on a section without titlePg — is the genuinely net-new case; a
/// `Default` create is refused in favor of `EditHeader`.)
#[test]
fn create_header_is_faithful() {
    let (base, _) = doc_and_ids(&["Body paragraph one.", "Body paragraph two."]);
    // The importer synthesizes a blank Default header reference; there is no
    // Even reference yet — the precondition for a net-new Even create.
    let base_refs = body_header_refs(&base);
    assert!(
        !base_refs.iter().any(|(k, _)| *k == HeaderFooterKind::Even),
        "base must have no Even header reference"
    );

    let steps = vec![EditStep::CreateHeader {
        kind: HeaderFooterKind::Even,
        rationale: None,
    }];

    // (a) Generic gate: body content reverses, opaque inventory non-shrinking.
    assert_fidelity("create_header", &base, steps.clone());

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;

    // The tracked edit added exactly ONE new Even reference + story on top of
    // whatever the importer synthesized, recorded as a tracked sectPrChange.
    assert!(
        tracked.body_section_property_change.is_some(),
        "the reference is authored as a tracked w:sectPrChange"
    );
    let tracked_refs = body_header_refs(&tracked);
    let even_ref = tracked_refs
        .iter()
        .find(|(k, _)| *k == HeaderFooterKind::Even)
        .expect("an Even header reference was added");
    // Exactly one net-new story+ref relative to base.
    assert_eq!(
        tracked.headers.len(),
        base.headers.len() + 1,
        "exactly one net-new header story is created"
    );
    assert_eq!(
        tracked_refs.len(),
        base_refs.len() + 1,
        "exactly one net-new header reference is added"
    );
    // The blank story carries no visible text.
    let new_part = &even_ref.1;
    let story = tracked
        .headers
        .iter()
        .find(|h| &h.part_name == new_part)
        .expect("new Even story present");
    let visible: String = story
        .blocks
        .iter()
        .filter_map(|b| match &b.block {
            BlockNode::Paragraph(p) => Some(
                p.segments
                    .iter()
                    .flat_map(|s| s.inlines.iter())
                    .filter_map(|i| match i {
                        InlineNode::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect();
    assert!(
        visible.trim().is_empty(),
        "the new header story starts blank"
    );

    // (b1) Reversibility on the IR: reject-all restores the original — the new
    //      Even story+ref are gone (the orphan blank story is pruned), and every
    //      pre-existing (synthesized) reference + story is intact.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        header_part_names(&rejected),
        header_part_names(&base),
        "reject-all must drop the net-new header story and keep the original ones"
    );
    assert_eq!(
        body_header_refs(&rejected),
        body_header_refs(&base),
        "reject-all must restore the original section references exactly"
    );
    assert!(
        rejected.body_section_property_change.is_none(),
        "reject-all must clear the tracked sectPrChange"
    );

    // (b2) Accept-all equals direct apply on the header story + reference.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        header_part_names(&accepted),
        header_part_names(&direct),
        "accept-all header stories must equal direct apply"
    );
    assert_eq!(
        body_header_refs(&accepted),
        body_header_refs(&direct),
        "accept-all header references must equal direct apply"
    );
    assert!(
        direct.body_section_property_change.is_none(),
        "direct apply records no sectPrChange"
    );
}

/// The footer twin: a net-new `Even` footer with the same reversibility /
/// accept==direct guarantees over `doc.footers` / `footer_refs`.
#[test]
fn create_footer_is_faithful() {
    let (base, _) = doc_and_ids(&["Body paragraph one."]);
    let base_footer_refs = |c: &CanonDoc| -> Vec<(HeaderFooterKind, String)> {
        c.body_section_properties
            .as_ref()
            .map(|sp| {
                sp.footer_refs
                    .iter()
                    .map(|r| (r.kind.clone(), r.part_path.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let base_refs = base_footer_refs(&base);
    assert!(
        !base_refs.iter().any(|(k, _)| *k == HeaderFooterKind::Even),
        "base must have no Even footer reference"
    );

    let steps = vec![EditStep::CreateFooter {
        kind: HeaderFooterKind::Even,
        rationale: None,
    }];

    assert_fidelity("create_footer", &base, steps.clone());

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    assert_eq!(
        tracked.footers.len(),
        base.footers.len() + 1,
        "exactly one net-new footer story is created"
    );
    assert!(
        base_footer_refs(&tracked)
            .iter()
            .any(|(k, _)| *k == HeaderFooterKind::Even),
        "an Even footer reference was added"
    );

    // Reject restores base exactly.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        rejected.footers.len(),
        base.footers.len(),
        "reject-all drops the net-new footer story"
    );
    assert_eq!(
        base_footer_refs(&rejected),
        base_refs,
        "reject-all restores the original footer references exactly"
    );

    // Accept equals direct.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        accepted
            .footers
            .iter()
            .map(|f| f.part_name.clone())
            .collect::<Vec<_>>(),
        direct
            .footers
            .iter()
            .map(|f| f.part_name.clone())
            .collect::<Vec<_>>(),
        "accept-all footer stories must equal direct apply"
    );
}
