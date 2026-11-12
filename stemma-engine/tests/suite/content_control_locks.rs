//! A content control's lock decides whether its contents can be edited, the
//! same way on every route that edits them.
//!
//! Domain rule (ECMA-376 §17.5.2.23 `w:lock`): `contentLocked` forbids editing
//! the control's contents, `sdtContentLocked` forbids that and deleting the
//! control, `sdtLocked` only forbids deleting the control (its contents stay
//! editable), and `unlocked` locks nothing. A fill, a set value, an in-place
//! text edit and a text replacement of a control whose contents are locked
//! refuse, naming the lock; each of them edits an `sdtLocked` control.

use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
use stemma::edit::{
    BarrierPolicy, EditError, EditStep, EditTransaction, ExpectedMatches, MatchMode,
    MaterializationMode, ReplaceTextError, ReplaceTextOptions, ReplaceTextScope, SdtValue,
    StoryRef, plan_replace_text,
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

/// A paragraph holding an inline plain-text control, then a block control
/// over one paragraph; both carry `w:lock` with `lock`.
fn locked(lock: &str) -> Document {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Tenant: </w:t></w:r><w:sdt><w:sdtPr><w:lock w:val="{lock}"/><w:id w:val="5"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t>Orchard Lane Holdings</w:t></w:r></w:sdtContent></w:sdt></w:p><w:sdt><w:sdtPr><w:lock w:val="{lock}"/><w:id w:val="6"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Orchard Lane Estate</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p/>"#
    );
    Document::parse(&docx(&body)).expect("parse")
}

/// (host paragraph, inline control, the block control's paragraph).
fn ids(doc: &Document) -> (String, String, String) {
    let blocks = &doc.snapshot().canonical.blocks;
    let BlockNode::Paragraph(host) = &blocks[0].block else {
        panic!("the body starts with the host paragraph")
    };
    let control = host
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .find_map(|i| match i {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Sdt) => {
                Some(o.id.to_string())
            }
            _ => None,
        })
        .expect("an inline control");
    let BlockNode::Paragraph(block_value) = &blocks[1].block else {
        panic!("the block control holds a paragraph")
    };
    assert!(
        blocks[1].block_sdt_wrap.is_some(),
        "the block control is open"
    );
    (host.id.to_string(), control, block_value.id.to_string())
}

/// Every route that edits a control's contents, as one step each.
fn routes(doc: &Document) -> Vec<(&'static str, EditStep)> {
    let (host, control, block_value) = ids(doc);
    vec![
        (
            "set_content_control_value",
            EditStep::SetContentControlValue {
                block_id: host.clone().into(),
                sdt_id: control.clone().into(),
                value: SdtValue::Text("Birch Row Holdings".to_string()),
                tracked: false,
                rationale: None,
            },
        ),
        (
            "opaque_text_edit",
            EditStep::OpaqueTextEdit {
                story: StoryRef::Body,
                block_id: host.clone().into(),
                opaque_id: control.clone().into(),
                container_index: 0,
                paragraph_index: 0,
                find: "Orchard Lane".to_string(),
                replacement: "Birch Row".to_string(),
                semantic_hash: None,
                rationale: None,
            },
        ),
        (
            "sdt_text_fill inline",
            EditStep::SdtTextFill {
                block_id: Some(host.into()),
                sdt_id: Some(control.into()),
                body_index: None,
                value: "Birch Row Holdings".to_string(),
                semantic_hash: None,
                rationale: None,
            },
        ),
        (
            "sdt_text_fill block",
            EditStep::SdtTextFill {
                block_id: Some(block_value.into()),
                sdt_id: None,
                body_index: None,
                value: "Birch Row Estate".to_string(),
                semantic_hash: None,
                rationale: None,
            },
        ),
    ]
}

fn apply(doc: &Document, step: EditStep) -> Result<Document, String> {
    doc.apply(&EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 0,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: None,
            apply_op_id: None,
        },
    })
    .map_err(|e| format!("{:?}: {}", e.code, e.message))
}

fn replace(old: &str) -> ReplaceTextOptions {
    ReplaceTextOptions {
        old: old.to_string(),
        new: "Birch Row".to_string(),
        author: "Reviewer".to_string(),
        scope: ReplaceTextScope::BodyAndTables,
        expected: ExpectedMatches::All,
        match_mode: MatchMode::Exact,
        on_barrier_match: BarrierPolicy::Fail,
    }
}

#[test]
fn every_route_refuses_a_control_whose_contents_are_locked() {
    for lock in ["contentLocked", "sdtContentLocked"] {
        let doc = locked(lock);
        for (route, step) in routes(&doc) {
            let Err(error) = apply(&doc, step) else {
                panic!("{lock} {route}: the edit applied")
            };
            assert!(
                error.starts_with("UnsupportedEdit")
                    && error.contains(&format!("w:lock=\"{lock}\"")),
                "{lock} {route}: {error}"
            );
        }
        // Text replacement inside the block control's paragraph.
        match plan_replace_text(&doc.snapshot().canonical, &replace("Orchard Lane Estate")) {
            Err(ReplaceTextError::Engine(EditError::ContentControlLocked {
                lock: found, ..
            })) => {
                assert_eq!(found, lock)
            }
            other => panic!("{lock}: expected the lock refusal, got {other:?}"),
        }
    }
}

#[test]
fn every_route_edits_a_control_locked_only_against_deletion() {
    let doc = locked("sdtLocked");
    for (route, step) in routes(&doc) {
        let edited = apply(&doc, step).unwrap_or_else(|e| panic!("{route}: {e}"));
        assert!(!edited.revisions().is_empty() || route == "set_content_control_value");
    }
    let plan = plan_replace_text(&doc.snapshot().canonical, &replace("Orchard Lane"))
        .expect("an sdtLocked control's contents are replaced");
    assert_eq!(
        plan.matches.len(),
        2,
        "the inline value and the block paragraph"
    );
}

/// A block control whose content holds a `w:customXml` element directly (no
/// block model slot for it), so it imports sealed and is filled by body
/// index; it carries `w:lock` with `lock`.
fn sealed_locked(lock: &str) -> Document {
    let body = format!(
        r#"<w:sdt><w:sdtPr><w:lock w:val="{lock}"/><w:id w:val="7"/><w:alias w:val="Term"/></w:sdtPr><w:sdtContent><w:customXml w:uri="urn:x" w:element="term"><w:p><w:r><w:t>Twelve months</w:t></w:r></w:p></w:customXml></w:sdtContent></w:sdt><w:p/>"#
    );
    Document::parse(&docx(&body)).expect("parse")
}

fn sealed_fill(doc: &Document) -> EditStep {
    let body_index = doc
        .snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::OpaqueBlock(o) if matches!(o.kind, OpaqueKind::Sdt) => o
                .proof_ref
                .docx_anchor
                .strip_prefix("body_index:")
                .map(|n| n.parse::<usize>().unwrap()),
            _ => None,
        })
        .expect("a sealed block control");
    EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(body_index),
        value: "Eighteen months".to_string(),
        semantic_hash: None,
        rationale: None,
    }
}

#[test]
fn a_fill_of_a_sealed_control_whose_contents_are_locked_is_refused_as_an_edit() {
    for lock in ["contentLocked", "sdtContentLocked"] {
        let doc = sealed_locked(lock);
        // The edit is refused, by the typed lock refusal every other route
        // gives (naming its step and the lock), in both modes.
        for mode in [
            MaterializationMode::TrackedChange,
            MaterializationMode::Direct,
        ] {
            let error = doc
                .apply(&EditTransaction {
                    steps: vec![sealed_fill(&doc)],
                    summary: None,
                    materialization_mode: mode,
                    revision: RevisionInfo {
                        revision_id: 0,
                        identity: 0,
                        author: Some("Reviewer".to_string()),
                        date: None,
                        apply_op_id: None,
                    },
                })
                .err()
                .unwrap_or_else(|| panic!("{lock} {mode:?}: the fill applied"));
            assert!(
                matches!(error.code, stemma::ErrorCode::UnsupportedEdit)
                    && error.message.contains(&format!(
                        "step 0: content control 'opaque_1' is locked (w:lock=\"{lock}\")"
                    )),
                "{lock} {mode:?}: {error:?}"
            );
        }
    }
}

#[test]
fn a_fill_of_a_sealed_control_locked_only_against_deletion_applies_and_saves() {
    let doc = sealed_locked("sdtLocked");
    let edited =
        apply(&doc, sealed_fill(&doc)).expect("an sdtLocked control's contents are filled");
    edited
        .serialize(&stemma::ExportOptions::default())
        .expect("the filled document saves");
    assert!(!edited.revisions().is_empty());
}
