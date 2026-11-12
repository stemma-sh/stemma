//! A field is one object: `fldChar begin`, its instruction, `fldChar
//! separate`, its result and `fldChar end` (ECMA-376 §17.16.18). When a
//! comparison replaces a field whole, Word records one deletion of the old
//! field and one insertion of the new one, and resolving either revision
//! keeps every field whole. Stemma must do the same: the replacement is
//! exactly one deletion and one insertion, and resolving any single
//! revision, alone, leaves only whole fields in both readings, in memory
//! and after save and reopen.

use std::io::Write;

use stemma::api::{Document, ResolveSelectionAction};
use stemma::domain::{BlockNode, FieldKind, InlineNode, OpaqueKind, TrackingStatus};
use stemma::{ExportOptions, Resolution};

fn field(instruction: &str, result: &str) -> String {
    format!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> {instruction} </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>{result}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#
    )
}

fn docx(instruction: &str, result: &str) -> Vec<u8> {
    docx_with_line(&format!(
        r#"<w:r><w:t xml:space="preserve">Sheet </w:t></w:r>{}<w:r><w:t xml:space="preserve"> of the plan.</w:t></w:r>"#,
        field(instruction, result)
    ))
}

/// A two-paragraph document whose second paragraph holds `line` (runs).
fn docx_with_line(line: &str) -> Vec<u8> {
    package(&format!("<w:p>{line}</w:p>"))
}

/// A paragraph, then a one-cell table whose paragraph holds `line` (runs).
fn docx_with_cell_line(line: &str) -> Vec<u8> {
    package(&format!(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="6000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="6000" w:type="dxa"/></w:tcPr><w:p>{line}</w:p></w:tc></w:tr></w:tbl><w:p/>"#
    ))
}

/// A document of a first paragraph, then `blocks`.
fn package(blocks: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Planting schedule for the orchard.</w:t></w:r></w:p>{blocks}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn reopened(document: &Document) -> Document {
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    Document::parse(&bytes).expect("reopen")
}

/// One reading's paragraphs, a field written `{instruction|result}`.
/// Panics when a field is not whole: a begin without its end, an end
/// without its begin, or a separate outside a field.
fn reading(document: &Document) -> Vec<String> {
    assert!(
        document.revisions().is_empty(),
        "a reading has no revisions"
    );
    document
        .snapshot()
        .canonical
        .blocks
        .iter()
        .flat_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p) => vec![p],
            // A table's cell paragraphs, in order.
            BlockNode::Table(t) => t
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .flat_map(|cell| &cell.blocks)
                .filter_map(|block| match block {
                    BlockNode::Paragraph(p) => Some(p),
                    _ => None,
                })
                .collect(),
            BlockNode::OpaqueBlock(_) => Vec::new(),
        })
        .map(|p| {
            let mut out = String::new();
            let mut depth = 0usize;
            for inline in p.all_inlines() {
                match inline {
                    InlineNode::Text(t) => out.push_str(&t.text),
                    InlineNode::OpaqueInline(o) => {
                        if let OpaqueKind::Field(f) = &o.kind {
                            match f.field_kind {
                                FieldKind::Begin => {
                                    depth += 1;
                                    out.push('{');
                                }
                                FieldKind::Instruction => out.push_str(
                                    f.instruction_text.as_deref().unwrap_or_default().trim(),
                                ),
                                FieldKind::Separate => {
                                    assert!(depth > 0, "a separate outside a field: {out}");
                                    out.push('|');
                                }
                                FieldKind::End => {
                                    assert!(depth > 0, "a field end without its begin: {out}");
                                    depth -= 1;
                                    out.push('}');
                                }
                                FieldKind::Simple | FieldKind::Unknown(_) => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            assert_eq!(depth, 0, "a field begin without its end: {out}");
            out
        })
        .collect()
}

fn resolved(document: &Document, resolution: Resolution) -> Vec<String> {
    reading(&reopened(&document.project(resolution).expect("resolve")))
}

fn compare(base: &Document, target: &Document) -> Document {
    stemma_diff::diff_as(base, target, "compare").expect("comparison")
}

fn base_and_target() -> (Document, Document) {
    (
        Document::parse(&docx("PAGE", "4")).expect("parse base"),
        Document::parse(&docx("NUMPAGES", "9")).expect("parse target"),
    )
}

/// Word reads a whole-field replacement as one deletion and one insertion.
#[test]
fn a_replaced_field_is_one_deletion_and_one_insertion() {
    let (base, target) = base_and_target();
    {
        let (name, compare) = ("compare", compare);
        let redline = compare(&base, &target);
        let reopened_redline = reopened(&redline);
        for (state, doc) in [("in memory", &redline), ("reopened", &reopened_redline)] {
            // A revision is its identity; one revision may be carried by
            // several runs, each listed.
            let mut revisions: Vec<(String, u32)> = doc
                .revisions()
                .into_iter()
                .map(|r| (format!("{:?}", r.kind), r.revision_id))
                .collect();
            revisions.sort();
            revisions.dedup();
            let kinds: Vec<&str> = revisions.iter().map(|(kind, _)| kind.as_str()).collect();
            assert_eq!(
                kinds,
                vec!["Delete", "Insert"],
                "{name} {state}: {:?}",
                doc.revisions()
            );
        }
    }
}

/// Resolving any one revision alone keeps every field whole: the selected
/// document's readings hold only whole fields, and resolving the rest the
/// same way reads as the target (accepting) or the base (rejecting).
#[test]
fn resolving_one_field_revision_alone_keeps_the_fields_whole() {
    let (base, target) = base_and_target();
    let want_base = reading(&reopened(&base));
    let want_target = reading(&reopened(&target));
    {
        let (name, compare) = ("compare", compare);
        let redline = compare(&base, &target);
        let reopened_redline = reopened(&redline);
        for (state, doc) in [("in memory", &redline), ("reopened", &reopened_redline)] {
            for revision in doc.revisions() {
                let id = revision.revision_id;
                for (action, then, want) in [
                    (
                        ResolveSelectionAction::Accept,
                        Resolution::AcceptAll,
                        &want_target,
                    ),
                    (
                        ResolveSelectionAction::Reject,
                        Resolution::RejectAll,
                        &want_base,
                    ),
                ] {
                    let label = format!(
                        "{name} {state}: revision {id} ({:?}) {action:?} alone",
                        revision.kind
                    );
                    let one = doc
                        .project(Resolution::Selective {
                            ids: [id].into_iter().collect(),
                            action,
                        })
                        .unwrap_or_else(|e| panic!("{label}: {e:?}"));
                    let one = reopened(&one);
                    // Both readings of the partly resolved document hold
                    // whole fields (`reading` panics on a torn one).
                    resolved(&one, Resolution::AcceptAll);
                    resolved(&one, Resolution::RejectAll);
                    assert_eq!(&resolved(&one, then), want, "{label}, then the rest");
                }
            }
        }
    }
}

/// The fields of a tracked field replacement in the redline: every
/// structural part of one field carries one tracking status.
#[test]
fn a_tracked_field_has_one_status_for_all_its_parts() {
    let (base, target) = base_and_target();
    {
        let (name, compare) = ("compare", compare);
        let redline = reopened(&compare(&base, &target));
        let BlockNode::Paragraph(p) = &redline.snapshot().canonical.blocks[1].block else {
            panic!("the second block is a paragraph");
        };
        let mut open: Vec<u32> = Vec::new();
        for segment in &p.segments {
            let identity = match &segment.status {
                TrackingStatus::Inserted(r) | TrackingStatus::Deleted(r) => r.identity,
                _ => 0,
            };
            for inline in &segment.inlines {
                if let InlineNode::OpaqueInline(o) = inline
                    && let OpaqueKind::Field(f) = &o.kind
                {
                    match f.field_kind {
                        FieldKind::Begin => open.push(identity),
                        FieldKind::End => {
                            let begin = open.pop().expect("an end after its begin");
                            assert_eq!(
                                begin, identity,
                                "{name}: the field's begin and end are one revision"
                            );
                        }
                        _ => assert_eq!(
                            open.last().copied(),
                            Some(identity),
                            "{name}: every part of the field is the field's revision"
                        ),
                    }
                }
            }
        }
    }
}

/// Both readings of `redline` against the base's and the target's, in memory
/// and reopened, resolved all at once and revision by revision.
fn assert_laws(base: &Document, target: &Document, redline: &Document) {
    let want_base = reading(&reopened(base));
    let want_target = reading(&reopened(target));
    assert!(!redline.revisions().is_empty(), "the change is tracked");
    let reopened_redline = reopened(redline);
    for (state, doc) in [("in memory", redline), ("reopened", &reopened_redline)] {
        assert_eq!(
            resolved(doc, Resolution::RejectAll),
            want_base,
            "{state}: Reject All"
        );
        assert_eq!(
            resolved(doc, Resolution::AcceptAll),
            want_target,
            "{state}: Accept All"
        );
        let ids: std::collections::HashSet<u32> =
            doc.revisions().iter().map(|r| r.revision_id).collect();
        for (action, want) in [
            (ResolveSelectionAction::Reject, &want_base),
            (ResolveSelectionAction::Accept, &want_target),
        ] {
            let each = doc
                .project(Resolution::Selective {
                    ids: ids.clone(),
                    action,
                })
                .expect("resolve each revision");
            assert_eq!(&reading(&reopened(&each)), want, "{state}: {action:?} each");
        }
    }
}

/// A page number field that gives way to plain text is deleted whole, its
/// shown number with it: Reject All brings the field back.
#[test]
fn a_page_number_field_replaced_by_text_comes_back_on_reject() {
    let base = Document::parse(&docx_with_line(&format!(
        r#"<w:r><w:t xml:space="preserve">Sheet </w:t></w:r>{}<w:r><w:t xml:space="preserve"> of the plan.</w:t></w:r>"#,
        field("PAGE", "4")
    )))
    .expect("parse base");
    let target = Document::parse(&docx_with_line(
        r#"<w:r><w:t xml:space="preserve">Sheet four of the plan.</w:t></w:r>"#,
    ))
    .expect("parse target");
    assert_laws(&base, &target, &compare(&base, &target));
    assert_laws(&target, &base, &compare(&target, &base));
}

/// A field whose field code changed while its shown result stayed the same
/// is still a different field: the redline replaces it.
#[test]
fn a_field_whose_code_alone_changed_is_replaced() {
    for (from, to) in [
        ("PAGE", "NUMPAGES"),
        ("PAGE", "SECTIONPAGES"),
        ("DATE", "TIME"),
    ] {
        let base = Document::parse(&docx(from, "4")).expect("parse base");
        let target = Document::parse(&docx(to, "4")).expect("parse target");
        assert_laws(&base, &target, &compare(&base, &target));
    }
}

/// The same in a table cell, where the field's paragraph is compared within
/// its kept table.
#[test]
fn a_field_whose_code_alone_changed_in_a_table_cell_is_replaced() {
    let line = |instruction: &str| {
        format!(
            r#"<w:r><w:t xml:space="preserve">Sheet </w:t></w:r>{}<w:r><w:t xml:space="preserve"> of the plan.</w:t></w:r>"#,
            field(instruction, "4")
        )
    };
    let base = Document::parse(&docx_with_cell_line(&line("PAGE"))).expect("parse base");
    let target = Document::parse(&docx_with_cell_line(&line("NUMPAGES"))).expect("parse target");
    assert_laws(&base, &target, &compare(&base, &target));
}

/// A field replaced next to capitalised text whose stored letters render as
/// more letters (`ß` under caps reads `SS`): the redline still writes each
/// side's stored text, and replaces the field whole.
#[test]
fn a_field_replaced_beside_capitalised_text_keeps_the_stored_letters() {
    let line = |instruction: &str| {
        format!(
            r#"<w:r><w:rPr><w:caps/></w:rPr><w:t xml:space="preserve">Hauptstraße sheet </w:t></w:r>{}<w:r><w:t xml:space="preserve"> of the plan.</w:t></w:r>"#,
            field(instruction, "4")
        )
    };
    let base = Document::parse(&docx_with_line(&line("PAGE"))).expect("parse base");
    let target = Document::parse(&docx_with_line(&line("NUMPAGES"))).expect("parse target");
    assert_laws(&base, &target, &compare(&base, &target));
}
