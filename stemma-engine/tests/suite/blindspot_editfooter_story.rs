//! Blindspot regression: `EditStep::EditFooter` / `StoryRef::Footer` end to end.
//!
//! The footer-editing twin of `headers_footers_edit.rs` was never exercised:
//! no test built a `StoryRef::Footer` edit, applied it, asserted accept/reject
//! correctness, AND proved the serialized text lands in `word/footerN.xml`.
//!
//! Domain-correct behavior (ECMA-376 §17.10 footer; the `EditFooter` contract
//! in `stemma-engine/src/edit/mod.rs:754` and `verbs/headers_footers.rs`):
//!  1. an `EditFooter` is a TRACKED, story-scoped text edit of a footer-story
//!     paragraph — reject-all restores the original footer text, accept-all
//!     keeps the edited text, and the body is never touched;
//!  2. on serialize the edited text must land in the footer part
//!     (`word/footer1.xml`) — not the body (`word/document.xml`), not a header.
//!
//! If footer editing misroutes (resolves the wrong story, mutates the body, or
//! serializes the text into the wrong part) this test FAILS and pinpoints the
//! defect. If it is correct the gap is now covered.

use std::io::Read as _;

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, NodeId, RevisionInfo};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent, StoryRef,
};
use stemma::{accept_all, reject_all_with_styles};

/// Build a DOCX with a body paragraph, a `footer1.xml` part (Default kind) whose
/// single paragraph holds `footer_text`, and a `footerReference` in the body
/// sectPr. Mirrors `make_header_docx` from `headers_footers_edit.rs` but for the
/// footer part / relationship type / content type.
fn make_footer_docx(footer_text: &str) -> Vec<u8> {
    let footer_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t xml:space="preserve">{footer_text}</w:t></w:r></w:p></w:ftr>"#
    );

    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>
<w:sectPr>
<w:footerReference w:type="default" r:id="rIdF1"/>
<w:pgSz w:w="12240" w:h="15840"/>
</w:sectPr>
</w:body></w:document>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;

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
        zip.start_file("word/footer1.xml", opts).unwrap();
        zip.write_all(footer_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Tester".to_string()),
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

/// Visible text of a footer story's first paragraph (Text inlines only).
fn footer_text(canon: &stemma::domain::CanonDoc, part: &str) -> String {
    let story = canon
        .footers
        .iter()
        .find(|f| f.part_name == part)
        .expect("footer story present");
    story
        .blocks
        .iter()
        .flat_map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.segments.clone(),
            _ => vec![],
        })
        .flat_map(|s| s.inlines)
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

fn body_text(canon: &stemma::domain::CanonDoc) -> String {
    canon
        .blocks
        .iter()
        .flat_map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.segments.clone(),
            _ => vec![],
        })
        .flat_map(|s| s.inlines)
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

/// The footer story's part name + first paragraph block id.
fn footer_addr(canon: &stemma::domain::CanonDoc) -> (String, NodeId) {
    let story = canon.footers.first().expect("footer story present");
    let block_id = story
        .blocks
        .iter()
        .find_map(|b| match &b.block {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        })
        .expect("footer has a paragraph block");
    (story.part_name.clone(), block_id)
}

/// Read one part's bytes (as a string) out of a serialized DOCX zip.
fn part_text(docx: &[u8], part: &str) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip open");
    let mut file = zip.by_name(part).ok()?;
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read part");
    Some(s)
}

/// List the part names present in a serialized DOCX (for routing assertions).
fn part_names(docx: &[u8]) -> Vec<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip open");
    (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect()
}

/// Concatenate the text inside every `<w:t ...>...</w:t>` element of a part.
/// This reads the part's *visible run text* independent of OOXML-irrelevant
/// run-splitting (the word-level diff may emit "Serialized Footer" as two `w:t`
/// runs); the domain-correct fact under test is which part the text lands in,
/// not how the runs are segmented.
fn visible_t_text(part_xml: &str) -> String {
    let mut out = String::new();
    let mut rest = part_xml;
    while let Some(open) = rest.find("<w:t") {
        // Skip past the opening tag (handles `<w:t>` and `<w:t xml:space=...>`).
        let after_open = &rest[open..];
        let Some(gt) = after_open.find('>') else {
            break;
        };
        let content_start = open + gt + 1;
        let Some(close_rel) = rest[content_start..].find("</w:t>") else {
            break;
        };
        out.push_str(&rest[content_start..content_start + close_rel]);
        rest = &rest[content_start + close_rel + "</w:t>".len()..];
    }
    out
}

/// (1) A tracked `EditFooter` reverses on reject and applies on accept, and the
/// edit is footer-story-scoped — the body text never changes.
#[test]
fn tracked_edit_footer_reject_restores_accept_keeps_and_body_untouched() {
    let doc = Document::parse(&make_footer_docx("Draft Footer")).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let (part, block_id) = footer_addr(&base);
    assert_eq!(
        footer_text(&base, &part),
        "Draft Footer",
        "fixture: footer holds the original text"
    );
    assert_eq!(part, "footer1.xml", "fixture: footer part is footer1.xml");
    let base_body = body_text(&base);

    let steps = vec![EditStep::EditFooter {
        story: StoryRef::Footer(part.clone()),
        block_id,
        expect: "Draft Footer".to_string(),
        semantic_hash: None,
        content: text_content("Final Footer"),
        rationale: None,
    }];

    let tracked = doc
        .apply(&txn(steps, MaterializationMode::TrackedChange))
        .expect("tracked edit_footer applies")
        .snapshot()
        .canonical
        .clone();

    // reject-all restores the original footer text.
    let mut rejected = (*tracked).clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        footer_text(&rejected, &part),
        "Draft Footer",
        "reject-all restores the original footer text"
    );

    // accept-all keeps the edited footer text.
    let mut accepted = (*tracked).clone();
    accept_all(&mut accepted);
    assert_eq!(
        footer_text(&accepted, &part),
        "Final Footer",
        "accept-all keeps the edited footer text"
    );

    // The body never changes (story-scoped edit), on both projections.
    assert_eq!(body_text(&tracked), base_body, "body untouched (tracked)");
    assert_eq!(body_text(&accepted), base_body, "body untouched (accepted)");
}

/// (2) On serialize, the edited footer text lands in `word/footer1.xml` — not
/// the body, not a header. We accept-all so a single clean run carries the new
/// text, then serialize and unzip.
#[test]
fn edited_footer_text_serializes_into_footer_part_not_body() {
    let doc = Document::parse(&make_footer_docx("Draft Footer")).expect("parse");
    let (part, block_id) = footer_addr(&doc.snapshot().canonical);

    let steps = vec![EditStep::EditFooter {
        story: StoryRef::Footer(part.clone()),
        block_id,
        expect: "Draft Footer".to_string(),
        semantic_hash: None,
        content: text_content("Serialized Footer"),
        rationale: None,
    }];

    // Apply, then accept-all via the public projection so the footer holds a
    // single clean run with the new text (avoids matching the original text
    // inside a tracked-delete that Redline mode would also emit).
    let edited = doc
        .apply(&txn(steps, MaterializationMode::TrackedChange))
        .expect("tracked edit_footer applies");
    let accepted = edited
        .project(stemma::Resolution::AcceptAll)
        .expect("accept-all projects");

    let bytes = accepted
        .serialize(&stemma::runtime::ExportOptions::default())
        .expect("serialize accepted footer edit");

    let names = part_names(&bytes);
    assert!(
        names.iter().any(|n| n == "word/footer1.xml"),
        "serialized package keeps the footer part; parts = {names:?}"
    );

    let footer_part = part_text(&bytes, "word/footer1.xml")
        .expect("word/footer1.xml present in serialized package");
    assert_eq!(
        visible_t_text(&footer_part),
        "Serialized Footer",
        "edited footer text (visible run text) lands in word/footer1.xml; raw: {footer_part}"
    );

    // It must NOT leak into the body.
    let body_part = part_text(&bytes, "word/document.xml").expect("word/document.xml present");
    assert!(
        !visible_t_text(&body_part).contains("Serialized"),
        "edited footer text must NOT appear in the body (word/document.xml); got: {body_part}"
    );
    assert!(
        visible_t_text(&body_part).contains("Body paragraph."),
        "body paragraph text is preserved in the body"
    );

    // It must NOT leak into any header part.
    for name in &names {
        if name.starts_with("word/header") && name.ends_with(".xml") {
            let header_part = part_text(&bytes, name).unwrap_or_default();
            assert!(
                !visible_t_text(&header_part).contains("Serialized"),
                "edited footer text must NOT appear in a header part ({name})"
            );
        }
    }
}
