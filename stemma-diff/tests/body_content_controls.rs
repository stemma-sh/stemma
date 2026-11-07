//! Block content controls (`w:sdt`, §17.5.2) have no revision, so a redline
//! keeps the controls both readings agree on: Reject All shows the source's
//! blocks in the source's controls and Accept All the target's blocks in the
//! target's controls. The one thing Word cannot do is remove a control by
//! resolving revisions: a control added or removed whole stays, emptied, in
//! the reading that lacks it (checked against Word 16, which shows the empty
//! control's placeholder).

use std::io::Write;

use stemma::api::Document;
use stemma::domain::BlockNode;
use stemma::{ExportOptions, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
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

fn paragraph(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"#)
}

fn control(id: &str, content: &str) -> String {
    format!(
        r#"<w:sdt><w:sdtPr><w:alias w:val="Control {id}"/><w:id w:val="{id}"/></w:sdtPr><w:sdtContent>{content}</w:sdtContent></w:sdt>"#
    )
}

/// The body as (block text, the alias of each enclosing control).
fn reading(document: &Document) -> Reading {
    let canonical = &document.snapshot().canonical;
    let controls = stemma::block_controls::membership(&canonical.blocks);
    canonical
        .blocks
        .iter()
        .map(|tracked| {
            let text = match &tracked.block {
                BlockNode::Paragraph(paragraph) => paragraph
                    .all_inlines()
                    .filter_map(|inline| match inline {
                        stemma::domain::InlineNode::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect(),
                BlockNode::Table(_) => "[table]".to_string(),
                BlockNode::OpaqueBlock(_) => "[opaque]".to_string(),
            };
            let aliases = controls.paths[stemma::block_controls::tracked_block_id(tracked)]
                .iter()
                .map(|&instance| {
                    let sdt_pr = String::from_utf8_lossy(&controls.controls[instance].sdt_pr_xml)
                        .into_owned();
                    sdt_pr
                        .split("w:alias w:val=\"")
                        .nth(1)
                        .and_then(|rest| rest.split('"').next())
                        .expect("every fixture control has an alias")
                        .to_string()
                })
                .collect();
            (text, aliases)
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

/// Compare, save, reopen, and check both readings against the inputs.
fn assert_readings_reproduce_inputs(base_body: &str, target_body: &str) -> Document {
    let base = Document::parse(&docx(base_body)).expect("parse base");
    let target = Document::parse(&docx(target_body)).expect("parse target");
    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));
    assert_eq!(
        reading(&reopened(
            &redline.project(Resolution::AcceptAll).expect("accept")
        )),
        reading(&target),
        "Accept All gives the target, controls included"
    );
    assert_eq!(
        reading(&reopened(
            &redline.project(Resolution::RejectAll).expect("reject")
        )),
        reading(&base),
        "Reject All gives the source, controls included"
    );
    redline
}

#[test]
fn an_edit_inside_an_unchanged_control_is_a_revision_inside_it() {
    let redline = assert_readings_reproduce_inputs(
        &format!(
            "{}{}",
            control("7", &format!("{}{}", paragraph("Name"), paragraph("Old"))),
            paragraph("After")
        ),
        &format!(
            "{}{}",
            control("7", &format!("{}{}", paragraph("Name"), paragraph("New"))),
            paragraph("After")
        ),
    );
    // One control, the edit inside it: no copy of the control.
    let controls = stemma::block_controls::membership(&redline.snapshot().canonical.blocks);
    assert_eq!(controls.controls.len(), 1);
}

#[test]
fn replacing_all_of_a_controls_content_keeps_one_control() {
    let redline = assert_readings_reproduce_inputs(
        &format!(
            "{}{}",
            control("7", &paragraph("Entirely old wording here")),
            paragraph("After")
        ),
        &format!(
            "{}{}",
            control("7", &paragraph("Something unrelated and new")),
            paragraph("After")
        ),
    );
    let controls = stemma::block_controls::membership(&redline.snapshot().canonical.blocks);
    assert_eq!(
        controls.controls.len(),
        1,
        "the control holds both versions"
    );
}

#[test]
fn filling_a_placeholder_control_is_an_edit_inside_it() {
    // Word's control id names the control; filling it clears the placeholder
    // flag, so the rest of its properties differ.
    let placeholder = r#"<w:sdt><w:sdtPr><w:alias w:val="Name"/><w:id w:val="7"/><w:showingPlcHdr/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Click here to enter text.</w:t></w:r></w:p></w:sdtContent></w:sdt>"#;
    let filled = r#"<w:sdt><w:sdtPr><w:alias w:val="Name"/><w:id w:val="7"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Jane Doe</w:t></w:r></w:p></w:sdtContent></w:sdt>"#;
    let base = Document::parse(&docx(&format!("{placeholder}{}", paragraph("After"))))
        .expect("parse base");
    let target =
        Document::parse(&docx(&format!("{filled}{}", paragraph("After")))).expect("parse target");
    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));
    let controls = stemma::block_controls::membership(&redline.snapshot().canonical.blocks);
    assert_eq!(
        controls.controls.len(),
        1,
        "one control holds both versions"
    );
    // The control carries the target's properties: Accept All is not shown
    // as a placeholder.
    assert!(!String::from_utf8_lossy(&controls.controls[0].sdt_pr_xml).contains("showingPlcHdr"));
    for (resolution, expected) in [
        (Resolution::AcceptAll, "Jane Doe"),
        (Resolution::RejectAll, "Click here to enter text."),
    ] {
        assert_eq!(
            reading(&reopened(&redline.project(resolution).expect("resolve"))),
            vec![
                (expected.to_string(), vec!["Name".to_string()]),
                ("After".to_string(), vec![])
            ]
        );
    }
}

/// Compare, save, reopen, and return the (Accept All, Reject All) readings.
fn resolved_readings(base_body: &str, target_body: &str) -> (Reading, Reading) {
    let base = Document::parse(&docx(base_body)).expect("parse base");
    let target = Document::parse(&docx(target_body)).expect("parse target");
    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));
    (
        reading(&reopened(
            &redline.project(Resolution::AcceptAll).expect("accept"),
        )),
        reading(&reopened(
            &redline.project(Resolution::RejectAll).expect("reject"),
        )),
    )
}

type Reading = Vec<(String, Vec<String>)>;

fn row(text: &str, controls: &[&str]) -> (String, Vec<String>) {
    (
        text.to_string(),
        controls.iter().map(|alias| alias.to_string()).collect(),
    )
}

#[test]
fn a_control_added_around_shared_text_stays_emptied_on_reject() {
    let (accepted, rejected) = resolved_readings(
        &format!("{}{}", paragraph("Name"), paragraph("After")),
        &format!("{}{}", control("7", &paragraph("Name")), paragraph("After")),
    );
    assert_eq!(
        accepted,
        vec![row("Name", &["Control 7"]), row("After", &[])]
    );
    assert_eq!(
        rejected,
        vec![row("", &["Control 7"]), row("Name", &[]), row("After", &[])]
    );
}

#[test]
fn a_control_removed_from_around_shared_text_stays_emptied_on_accept() {
    let (accepted, rejected) = resolved_readings(
        &format!("{}{}", control("7", &paragraph("Name")), paragraph("After")),
        &format!("{}{}", paragraph("Name"), paragraph("After")),
    );
    assert_eq!(
        accepted,
        vec![row("Name", &[]), row("", &["Control 7"]), row("After", &[])]
    );
    assert_eq!(
        rejected,
        vec![row("Name", &["Control 7"]), row("After", &[])]
    );
}

#[test]
fn an_added_control_keeps_its_content_and_stays_emptied_on_reject() {
    let (accepted, rejected) = resolved_readings(
        &paragraph("After"),
        &format!(
            "{}{}",
            control("7", &format!("{}{}", paragraph("One"), paragraph("Two"))),
            paragraph("After")
        ),
    );
    assert_eq!(
        accepted,
        vec![
            row("One", &["Control 7"]),
            row("Two", &["Control 7"]),
            row("After", &[])
        ]
    );
    assert_eq!(rejected, vec![row("", &["Control 7"]), row("After", &[])]);
}

#[test]
fn nested_controls_keep_their_shape() {
    assert_readings_reproduce_inputs(
        &format!(
            "{}{}",
            control(
                "1",
                &format!(
                    "{}{}",
                    paragraph("Intro"),
                    control("2", &paragraph("Inner old"))
                )
            ),
            paragraph("After")
        ),
        &format!(
            "{}{}",
            control(
                "1",
                &format!(
                    "{}{}",
                    paragraph("Intro"),
                    control("2", &paragraph("Inner new"))
                )
            ),
            paragraph("After")
        ),
    );
}
