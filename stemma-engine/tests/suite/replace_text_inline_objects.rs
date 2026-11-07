//! Text replacement never reports success while an occurrence it was asked to
//! replace stands inside a hyperlink or a content control.
//!
//! Domain rule: a hyperlink's display text (§17.16.22 `w:hyperlink`) and an
//! inline content control's value (§17.5.2.36 `w:sdtContent`) are text of the
//! paragraph that the reader sees, but a replace-text splice edits the
//! paragraph's own runs; those occurrences are edited through their objects
//! (`replace` on the hyperlink, `set_content_control_value` / `opaque_text_edit`
//! on the control). A find/replace whose scope holds such an occurrence must
//! refuse and name each object, not replace the rest and call it complete —
//! with a counted `expected_matches` and with "replace all" alike.

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
use stemma::edit::{
    BarrierPolicy, EditError, ExpectedMatches, FindReplaceOptions, FindReplaceScope,
    InlineObjectText, MatchMode, ReplaceTextError, ReplaceTextOptions, ReplaceTextScope,
    plan_find_replace_all, plan_replace_text,
};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

const LINK: &str = r#"<w:p><w:r><w:t xml:space="preserve">Acme Corp signs; see </w:t></w:r><w:hyperlink w:anchor="terms"><w:r><w:t>the Acme Corp terms</w:t></w:r></w:hyperlink><w:r><w:t>.</w:t></w:r></w:p>"#;

fn control(value: &str) -> String {
    format!(
        r#"<w:sdt><w:sdtPr><w:id w:val="7"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t>{value}</w:t></w:r></w:sdtContent></w:sdt>"#
    )
}

/// The ids of every inline object of `kind` in the body, cells included, in
/// document order: (paragraph id, object id).
fn objects(doc: &Document, hyperlink: bool) -> Vec<(String, String)> {
    fn walk(block: &BlockNode, hyperlink: bool, out: &mut Vec<(String, String)>) {
        match block {
            BlockNode::Paragraph(p) => {
                for inline in p.segments.iter().flat_map(|s| &s.inlines) {
                    if let InlineNode::OpaqueInline(o) = inline {
                        let wanted = match &o.kind {
                            OpaqueKind::Hyperlink(_) => hyperlink,
                            OpaqueKind::Sdt => !hyperlink,
                            _ => false,
                        };
                        if wanted {
                            out.push((p.id.to_string(), o.id.to_string()));
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for cell in t.rows.iter().flat_map(|r| &r.cells) {
                    for b in &cell.blocks {
                        walk(b, hyperlink, out);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let snapshot = doc.snapshot();
    let mut out = Vec::new();
    for tracked in &snapshot.canonical.blocks {
        walk(&tracked.block, hyperlink, &mut out);
    }
    out
}

fn options(old: &str, expected: ExpectedMatches) -> ReplaceTextOptions {
    ReplaceTextOptions {
        old: old.to_string(),
        new: "Acme Inc".to_string(),
        author: "Reviewer".to_string(),
        scope: ReplaceTextScope::BodyAndTables,
        expected,
        match_mode: MatchMode::Exact,
        on_barrier_match: BarrierPolicy::Fail,
    }
}

/// The refusal's occurrences as (paragraph, object, kind).
fn refused_occurrences(
    result: Result<stemma::edit::ReplaceTextPlan, ReplaceTextError>,
) -> Vec<(String, String, InlineObjectText)> {
    match result {
        Err(ReplaceTextError::Engine(EditError::ReplaceTextInsideInlineObjects {
            occurrences,
            ..
        })) => occurrences
            .into_iter()
            .map(|o| (o.block_id.to_string(), o.object_id.to_string(), o.object))
            .collect(),
        Err(other) => panic!("expected the inline-object refusal, got {other:?}"),
        Ok(plan) => panic!(
            "replace_text must not succeed with an occurrence left in an object: {:?}",
            plan.matches
        ),
    }
}

#[test]
fn an_occurrence_in_link_text_refuses_counted_and_replace_all_items() {
    let doc = Document::parse(&docx(LINK)).expect("parse");
    let links = objects(&doc, true);
    assert_eq!(links.len(), 1, "the fixture holds one link");
    let canonical = doc.snapshot().canonical.clone();
    for expected in [ExpectedMatches::Count(1), ExpectedMatches::All] {
        let found = refused_occurrences(plan_replace_text(
            &canonical,
            &options("Acme Corp", expected),
        ));
        assert_eq!(
            found,
            vec![(
                links[0].0.clone(),
                links[0].1.clone(),
                InlineObjectText::HyperlinkText
            )],
            "{expected:?}"
        );
    }
    // The message names the object and the op that edits it.
    let Err(ReplaceTextError::Engine(error)) =
        plan_replace_text(&canonical, &options("Acme Corp", ExpectedMatches::All))
    else {
        panic!("refused")
    };
    let message = error.to_string();
    assert!(
        message.contains(&links[0].1) && message.contains("content.type=hyperlink"),
        "{message}"
    );
}

#[test]
fn an_occurrence_in_a_content_control_value_refuses_in_the_body_and_in_cells() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Party: </w:t></w:r>{}</w:p><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">Signed by Acme Corp as </w:t></w:r>{}</w:p></w:tc></w:tr></w:tbl><w:p/>"#,
        control("Acme Corp"),
        control("Acme Corp, Ltd")
    );
    let doc = Document::parse(&docx(&body)).expect("parse");
    let controls = objects(&doc, false);
    assert_eq!(controls.len(), 2, "the fixture holds two controls");
    let found = refused_occurrences(plan_replace_text(
        &doc.snapshot().canonical,
        &options("Acme Corp", ExpectedMatches::Count(1)),
    ));
    let expected: Vec<_> = controls
        .into_iter()
        .map(|(p, o)| (p, o, InlineObjectText::ContentControlValue))
        .collect();
    assert_eq!(found, expected);
}

#[test]
fn a_needle_absent_from_every_object_still_replaces() {
    let doc = Document::parse(&docx(LINK)).expect("parse");
    let plan = plan_replace_text(
        &doc.snapshot().canonical,
        &options("signs", ExpectedMatches::Count(1)),
    )
    .expect("no object holds the needle");
    assert_eq!(plan.matches.len(), 1);
}

#[test]
fn replace_all_paragraph_rewrite_refuses_an_occurrence_in_link_text() {
    let doc = Document::parse(&docx(LINK)).expect("parse");
    let links = objects(&doc, true);
    let result = plan_find_replace_all(
        &doc.snapshot().canonical,
        &FindReplaceOptions {
            needle: "Acme Corp".to_string(),
            replacement: "Acme Inc".to_string(),
            scope: FindReplaceScope::BodyOnly,
            case_sensitive: true,
            whole_word: false,
            on_barrier_match: BarrierPolicy::Fail,
        },
    );
    match result {
        Err(EditError::ReplaceTextInsideInlineObjects { occurrences, .. }) => {
            assert_eq!(occurrences.len(), 1);
            assert_eq!(occurrences[0].object_id.to_string(), links[0].1);
        }
        other => panic!("expected the inline-object refusal, got {other:?}"),
    }
}
