//! A one-word edit to a FORMATTED paragraph must produce a SURGICAL redline
//! (minimal del/ins) that preserves the authored marks — not a whole-paragraph
//! delete+insert.
//!
//! Before the fix, any replacement content carrying a mark (StyledText) routed to
//! the whole-paragraph segment-replace fallback (the materializer's word-diff
//! couldn't carry authored marks). Now the marks ride the word-diff via a
//! per-char map, so "test" -> bold "EXAM" yields:
//!   Normal "This is a " | Deleted "test" | Inserted bold "EXAM" | Normal " now foo bar baz"
//! and accept-all/reject-all hit the right endpoints with marks intact.
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

/// (status-tag, text, is_bold) for each text run of the first paragraph.
fn runs(canon: &CanonDoc) -> Vec<(&'static str, String, bool)> {
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
                    out.push((tag, t.text.clone(), t.marks.contains(&Mark::Bold)));
                }
            }
        }
    }
    out
}

fn para_text(canon: &CanonDoc) -> String {
    runs(canon).into_iter().map(|(_, t, _)| t).collect()
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Reviewer".to_string()),
            date: Some("2026-06-29T00:00:00Z".to_string()),
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

fn replace_step(id: NodeId, fragments: Vec<ContentFragment>) -> EditStep {
    EditStep::ReplaceParagraphText {
        block_id: id,
        rationale: None,
        replacement_role: None,
        expect: "test".to_string(),
        semantic_hash: None,
        content: ParagraphContent { fragments },
    }
}

#[test]
fn styled_one_word_replace_is_surgical_and_keeps_marks() {
    let doc = Document::parse(&make_docx("This is a test now foo bar baz")).unwrap();
    let id = first_block_id(&doc.snapshot().canonical);

    // "test" -> bold "EXAM", surrounding text unchanged but carried as STYLED-free
    // fragments (the realistic frontend shape: every run is a fragment).
    let edited = doc
        .apply(&txn(vec![replace_step(
            id,
            vec![
                ContentFragment::Text("This is a ".to_string()),
                ContentFragment::StyledText {
                    text: "EXAM".to_string(),
                    marks: bold(),
                },
                ContentFragment::Text(" now foo bar baz".to_string()),
            ],
        )]))
        .expect("styled replace applies");

    let r = runs(&edited.snapshot().canonical);
    // SURGICAL: exactly one Deleted "test" and one Inserted bold "EXAM"; the
    // surrounding text is Normal (NOT deleted+reinserted).
    let deleted: Vec<_> = r.iter().filter(|(s, _, _)| *s == "del").collect();
    let inserted: Vec<_> = r.iter().filter(|(s, _, _)| *s == "ins").collect();
    assert_eq!(deleted.len(), 1, "exactly one deleted run, got {r:?}");
    assert_eq!(deleted[0].1, "test", "deleted just the word, got {r:?}");
    assert_eq!(inserted.len(), 1, "exactly one inserted run, got {r:?}");
    assert_eq!(inserted[0].1, "EXAM");
    assert!(
        inserted[0].2,
        "the inserted run keeps its bold mark, got {r:?}"
    );
    assert!(
        r.iter()
            .any(|(s, t, _)| *s == "normal" && t == "This is a "),
        "leading text stays Normal (not part of the redline), got {r:?}"
    );
    assert!(
        r.iter()
            .any(|(s, t, _)| *s == "normal" && t == " now foo bar baz"),
        "trailing text stays Normal, got {r:?}"
    );

    // accept-all = the styled target, with the word bold.
    let acc = edited.project(Resolution::AcceptAll).expect("accept-all");
    assert_eq!(
        para_text(&acc.snapshot().canonical),
        "This is a EXAM now foo bar baz"
    );
    assert!(
        runs(&acc.snapshot().canonical)
            .iter()
            .any(|(_, t, b)| t.contains("EXAM") && *b),
        "accept-all keeps EXAM bold"
    );

    // reject-all = the original, plain.
    let rej = edited.project(Resolution::RejectAll).expect("reject-all");
    assert_eq!(
        para_text(&rej.snapshot().canonical),
        "This is a test now foo bar baz"
    );
    assert!(
        !runs(&rej.snapshot().canonical).iter().any(|(_, _, b)| *b),
        "reject-all leaves no stray bold"
    );
}
