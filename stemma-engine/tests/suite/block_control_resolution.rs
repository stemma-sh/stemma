//! A block content control (`w:sdt`, §17.5.2) has no revision of its own, so
//! editing and resolving inside it changes its content, never the control.
//! Every expectation here is what Word 16 does with the same redline: a
//! paragraph inserted inside a control belongs to it, and the control's last
//! paragraph mark cannot be resolved away (like the document-final mark).

use std::io::Write;

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::edit_v4::parse_transaction;
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::{ExportOptions, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

/// The body as (paragraph text, inside the control).
fn reading(document: &Document) -> Vec<(String, bool)> {
    let canonical = &document.snapshot().canonical;
    let mut inside = 0;
    canonical
        .blocks
        .iter()
        .map(|tracked| {
            if let Some(wrap) = &tracked.block_sdt_wrap {
                inside = wrap.span;
            }
            let in_control = inside > 0;
            inside = inside.saturating_sub(1);
            let stemma::domain::BlockNode::Paragraph(paragraph) = &tracked.block else {
                panic!("the fixture is paragraphs");
            };
            let text = paragraph
                .all_inlines()
                .filter_map(|inline| match inline {
                    stemma::domain::InlineNode::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect();
            (text, in_control)
        })
        .collect()
}

fn reopened(document: &Document) -> Document {
    Document::parse(
        &document
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen")
}

const TWO_IN_A_CONTROL: &str = r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:sdt><w:sdtPr><w:alias w:val="C"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>One</w:t></w:r></w:p><w:p><w:r><w:t>Two</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>After</w:t></w:r></w:p>"#;

fn row(text: &str, inside: bool) -> (String, bool) {
    (text.to_string(), inside)
}

/// One spelling: the saved redline reopens as the model that saved it. And
/// resolving the saved redline as Word does (on the package) gives the
/// model's own resolutions.
fn assert_saves_faithfully(edited: &Document, op: &str) {
    // One spelling: the saved redline reopens as the model that saved it.
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened(edited).snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{op}: {:?}",
        &differences[..differences.len().min(3)]
    );
    // Resolving the saved redline as Word does (on the package) gives the
    // model's own resolutions.
    let saved = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = DocxArchive::read(&saved).expect("read package");
    let (accepted_package, _) = normalize_docx(&archive).expect("package accept");
    let (rejected_package, _) = reject_all_docx(&archive).expect("package reject");
    for (label, package, resolution) in [
        ("accept", accepted_package, Resolution::AcceptAll),
        ("reject", rejected_package, Resolution::RejectAll),
    ] {
        let from_package = Document::parse(&package.write().expect("write")).expect("reopen");
        let from_model = reopened(&edited.project(resolution).expect("resolve"));
        let differences = stemma::roundtrip_compare::compare_canon_docs(
            &from_package.snapshot().canonical,
            &from_model.snapshot().canonical,
        );
        assert!(
            differences.is_empty(),
            "{op}: package {label} differs from model {label}: {:?}",
            &differences[..differences.len().min(3)]
        );
    }
}

/// Apply one tracked op (`{id}`/`{guard}` filled from the paragraph whose text
/// is `target`), then check Accept All and Reject All, before and after a
/// save and reopen of the redline.
fn check(op: &str, target: &str, accepted: &[(String, bool)], rejected: &[(String, bool)]) {
    check_in(TWO_IN_A_CONTROL, op, target, accepted, rejected);
}

fn check_in(
    body: &str,
    op: &str,
    target: &str,
    accepted: &[(String, bool)],
    rejected: &[(String, bool)],
) {
    let doc = Document::parse(&docx(body)).expect("parse");
    let view = doc.read();
    let block = view
        .blocks
        .iter()
        .find(|block| block.text == target)
        .expect("target paragraph");
    let mut op = op
        .replace("{id}", &block.id.to_string())
        .replace("{guard}", &block.guard);
    // `{id:Text}` names any other paragraph by its text.
    for other in &view.blocks {
        op = op.replace(&format!("{{id:{}}}", other.text), &other.id.to_string());
    }
    let transaction = |mode: &str| {
        parse_transaction(&format!(
            r#"{{"ops":[{op}],"materialization_mode":"{mode}","revision":{{"author":"R"}}}}"#
        ))
        .expect("schema")
        .into_edit_transaction()
        .expect("adapter")
    };
    // The same edit made directly reads as accepting it. A move exists only
    // as a tracked change.
    if !op.contains(r#""op":"move""#) {
        let direct = doc.apply(&transaction("direct")).expect("direct edit");
        assert_eq!(reading(&reopened(&direct)), accepted, "{op}: direct");
    }
    let edited = doc.apply(&transaction("tracked_change")).expect("edit");
    assert_saves_faithfully(&edited, &op);
    for redline in [reopened(&edited), edited] {
        assert_eq!(
            reading(&reopened(
                &redline.project(Resolution::AcceptAll).expect("accept")
            )),
            accepted
        );
        assert_eq!(
            reading(&reopened(
                &redline.project(Resolution::RejectAll).expect("reject")
            )),
            rejected
        );
    }
}

#[test]
fn a_paragraph_inserted_inside_a_control_belongs_to_it() {
    let source = [
        row("Before", false),
        row("One", true),
        row("Two", true),
        row("After", false),
    ];
    let insert = r#"{"op":"insert","target":{"anchor":"{id}","position":"after"},"content":[{"type":"paragraph","content":[{"type":"text","text":"New"}]}]}"#;
    check(
        insert,
        "One",
        &[
            row("Before", false),
            row("One", true),
            row("New", true),
            row("Two", true),
            row("After", false),
        ],
        &source,
    );
    // After the control's last paragraph: the next paragraph inside it.
    check(
        insert,
        "Two",
        &[
            row("Before", false),
            row("One", true),
            row("Two", true),
            row("New", true),
            row("After", false),
        ],
        &source,
    );
}

#[test]
fn a_paragraph_inserted_after_a_one_paragraph_control_belongs_to_it() {
    // The control's only paragraph keeps its content on Reject even though
    // its mark now carries the inserted break.
    let body = r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:sdt><w:sdtPr><w:alias w:val="C"/></w:sdtPr><w:sdtContent><w:p w14:paraId="1A2B3C4D" w:rsidR="00874ACF"><w:pPr><w:rPr><w:i/></w:rPr></w:pPr><w:r><w:t>One</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>After</w:t></w:r></w:p>"#;
    check_in(
        body,
        r#"{"op":"insert","target":{"anchor":"{id}","position":"after"},"content":[{"type":"paragraph","content":[{"type":"text","text":"New"}]}]}"#,
        "One",
        &[
            row("Before", false),
            row("One", true),
            row("New", true),
            row("After", false),
        ],
        &[row("Before", false), row("One", true), row("After", false)],
    );
}

#[test]
fn deleting_a_controls_last_paragraph_keeps_the_rest_of_the_control() {
    check(
        r#"{"op":"delete","target":"{id}","expect":"Two","guard":"{guard}"}"#,
        "Two",
        &[row("Before", false), row("One", true), row("After", false)],
        &[
            row("Before", false),
            row("One", true),
            row("Two", true),
            row("After", false),
        ],
    );
}

#[test]
fn resolving_a_deletion_inside_a_control_keeps_the_control() {
    let doc = Document::parse(&docx(
        r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:sdt><w:sdtPr><w:alias w:val="C"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>One</w:t></w:r></w:p><w:p><w:r><w:t>Two</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>After</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let view = doc.read();
    let one = view
        .blocks
        .iter()
        .find(|block| block.text == "One")
        .expect("the control's first paragraph");
    let transaction = parse_transaction(&format!(
        r#"{{"ops":[{{"op":"delete","target":"{}","expect":"One","guard":"{}"}}],"revision":{{"author":"R"}}}}"#,
        one.id, one.guard
    ))
    .expect("schema")
    .into_edit_transaction()
    .expect("adapter");
    let edited = doc.apply(&transaction).expect("delete");
    assert_eq!(
        reading(&reopened(
            &edited.project(Resolution::AcceptAll).expect("accept")
        )),
        vec![row("Before", false), row("Two", true), row("After", false)]
    );
    let accepted_after_save = reopened(&edited)
        .project(Resolution::AcceptAll)
        .expect("accept the saved redline");
    assert_eq!(
        reading(&reopened(&accepted_after_save)),
        vec![row("Before", false), row("Two", true), row("After", false)]
    );
    assert_eq!(
        reading(&reopened(
            &edited.project(Resolution::RejectAll).expect("reject")
        )),
        vec![
            row("Before", false),
            row("One", true),
            row("Two", true),
            row("After", false)
        ]
    );
}

#[test]
fn deleting_a_one_paragraph_controls_content_leaves_it_empty() {
    // Word keeps an emptied control: its last paragraph mark stays.
    let body = r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:sdt><w:sdtPr><w:alias w:val="C"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>One</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>After</w:t></w:r></w:p>"#;
    check_in(
        body,
        r#"{"op":"delete","target":"{id}","expect":"One","guard":"{guard}"}"#,
        "One",
        &[row("Before", false), row("", true), row("After", false)],
        &[row("Before", false), row("One", true), row("After", false)],
    );
}

#[test]
fn deleting_a_storys_last_paragraph_hands_its_mark_to_the_one_before() {
    // The story-final mark follows the same rule as a control's last mark.
    check_in(
        r#"<w:p><w:r><w:t>One</w:t></w:r></w:p><w:p><w:r><w:t>Two</w:t></w:r></w:p>"#,
        r#"{"op":"delete","target":"{id}","expect":"Two","guard":"{guard}"}"#,
        "Two",
        &[row("One", false)],
        &[row("One", false), row("Two", false)],
    );
}

/// A paragraph appended at the end of a story or a control takes over the
/// end's paragraph mark, so it takes over that mark's session attributes too:
/// Reject in Word merges the anchor into it, and nothing on the wire can
/// restore root `w:p` attributes. Its formatting comes from a role (here the
/// bold-marked paragraph), so its mark's formatting is tracked where Reject
/// restores it.
#[test]
fn an_appended_paragraph_takes_over_the_final_marks_session_attributes() {
    let zero = r#"<w:p w14:paraId="0A0C30F8" w:rsidR="00BD6B96" w:rsidP="00BF442E"><w:pPr><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:t>Zero</w:t></w:r></w:p>"#;
    let one = r#"<w:p w14:paraId="1A2B3C4D" w:rsidR="00874ACF"><w:r><w:t>One</w:t></w:r></w:p>"#;
    let op = r#"{"op":"insert","target":{"anchor":"{id}","position":"after"},"content":[{"type":"paragraph","role":"body_text","content":[{"type":"text","text":"New"}]}]}"#;
    check_in(
        &format!("{zero}{one}"),
        op,
        "One",
        &[row("Zero", false), row("One", false), row("New", false)],
        &[row("Zero", false), row("One", false)],
    );
    check_in(
        &format!(
            r#"{zero}<w:sdt><w:sdtPr><w:alias w:val="C"/></w:sdtPr><w:sdtContent>{one}</w:sdtContent></w:sdt><w:p><w:r><w:t>After</w:t></w:r></w:p>"#
        ),
        op,
        "One",
        &[
            row("Zero", false),
            row("One", true),
            row("New", true),
            row("After", false),
        ],
        &[row("Zero", false), row("One", true), row("After", false)],
    );
}

/// Resolving an unrelated revision keeps the rule at a control's last mark: a
/// paragraph appended in a control still ends it with an untracked mark.
#[test]
fn a_selective_resolution_keeps_an_appended_paragraphs_final_mark() {
    let doc = Document::parse(&docx(
        r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:sdt><w:sdtPr><w:alias w:val="C"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>One</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>After</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let view = doc.read();
    let id = |text: &str| {
        let block = view
            .blocks
            .iter()
            .find(|block| block.text == text)
            .expect("paragraph");
        (block.id.to_string(), block.guard.clone())
    };
    let (one, _) = id("One");
    let (before, before_guard) = id("Before");
    let transaction = |author: &str, op: String| {
        parse_transaction(&format!(
            r#"{{"ops":[{op}],"revision":{{"author":"{author}"}}}}"#
        ))
        .expect("schema")
        .into_edit_transaction()
        .expect("adapter")
    };
    let edited = doc
        .apply(&transaction(
            "A",
            format!(
                r#"{{"op":"insert","target":{{"anchor":"{one}","position":"after"}},"content":[{{"type":"paragraph","content":[{{"type":"text","text":"New"}}]}}]}}"#
            ),
        ))
        .expect("insert")
        .apply(&transaction(
            "B",
            format!(r#"{{"op":"delete","target":"{before}","expect":"Before","guard":"{before_guard}"}}"#),
        ))
        .expect("delete");
    let ids = edited
        .revisions()
        .into_iter()
        .filter(|revision| revision.author.as_deref() == Some("B"))
        .map(|revision| revision.revision_id)
        .collect();
    let resolved = edited
        .project(Resolution::Selective {
            ids,
            action: stemma::ResolveSelectionAction::Accept,
        })
        .expect("accept B");
    assert_saves_faithfully(&resolved, "accept B");
    assert_eq!(
        reading(&reopened(
            &resolved.project(Resolution::RejectAll).expect("reject")
        )),
        vec![row("One", true), row("After", false)]
    );
}

#[test]
fn a_tracked_move_across_a_controls_boundary_keeps_the_control_open() {
    // "Last" keeps the moved paragraphs off the story-final mark.
    let body = format!("{TWO_IN_A_CONTROL}<w:p><w:r><w:t>Last</w:t></w:r></w:p>");
    let source = [
        row("Before", false),
        row("One", true),
        row("Two", true),
        row("After", false),
        row("Last", false),
    ];
    let move_op = |anchor: &str, position: &str| {
        format!(
            r#"{{"op":"move","target":"{{id}}","destination":{{"anchor":"{{id:{anchor}}}","position":"{position}"}}}}"#
        )
    };
    // Out of the control.
    check_in(
        &body,
        &move_op("After", "after"),
        "One",
        &[
            row("Before", false),
            row("Two", true),
            row("After", false),
            row("One", false),
            row("Last", false),
        ],
        &source,
    );
    // Into the control.
    check_in(
        &body,
        &move_op("One", "after"),
        "After",
        &[
            row("Before", false),
            row("One", true),
            row("After", true),
            row("Two", true),
            row("Last", false),
        ],
        &source,
    );
    // Within it.
    check_in(
        &body,
        &move_op("Two", "after"),
        "One",
        &[
            row("Before", false),
            row("Two", true),
            row("One", true),
            row("After", false),
            row("Last", false),
        ],
        &source,
    );
}

#[test]
fn a_paragraph_inserted_before_a_controls_first_paragraph_comes_before_it() {
    let insert = r#"{"op":"insert","target":{"anchor":"{id}","position":"before"},"content":[{"type":"paragraph","content":[{"type":"text","text":"New"}]}]}"#;
    check(
        insert,
        "One",
        &[
            row("Before", false),
            row("New", false),
            row("One", true),
            row("Two", true),
            row("After", false),
        ],
        &[
            row("Before", false),
            row("One", true),
            row("Two", true),
            row("After", false),
        ],
    );
    // At the start of the story too.
    let body = r#"<w:sdt><w:sdtPr><w:alias w:val="C"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>One</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>After</w:t></w:r></w:p>"#;
    check_in(
        body,
        insert,
        "One",
        &[row("New", false), row("One", true), row("After", false)],
        &[row("One", true), row("After", false)],
    );
}
