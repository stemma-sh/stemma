//! Integration tests for content controls (Verb B): `WrapInContentControl`
//! (wrap a run-span in a `w:sdt`) and `SetContentControlValue` (mutate an
//! existing control's displayed value).
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; domain-model §11):
//!  - wrap produces an `OpaqueInline{Sdt}` whose `sdtPr` carries the
//!    tag/alias/control kind (dropdown items present), round-tripping through
//!    serialize → reparse;
//!  - set-value sets the displayed text / checkbox state and round-trips;
//!  - SDT structure is UNTRACKED — accept-all == reject-all == the edited doc;
//!  - fail-loud: `EmptyContentControlSpec`, `NotAContentControl`,
//!    `ContentControlTypeMismatch`.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::api::Document;
use stemma::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo, SdtControl, SdtListItem,
};
use stemma::edit::{
    EditError, EditStep, EditTransaction, MaterializationMode, SdtSpec, SdtValue, apply_transaction,
};
use stemma::{ExportMode, ExportOptions, Resolution, ValidatorLevel};

/// Minimal single-paragraph DOCX with the w14/w15 namespaces declared so a
/// content control (which may use `w14:checkbox` / `w15:repeatingSection`)
/// round-trips.
fn make_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
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
        _ => panic!("expected a paragraph"),
    }
}

/// The (paragraph id, sdt id, raw_xml) of the first content control found.
fn first_sdt(canon: &CanonDoc) -> Option<(NodeId, NodeId, String)> {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Sdt)
                    {
                        return Some((
                            p.id.clone(),
                            o.id.clone(),
                            String::from_utf8(o.raw_xml.clone().unwrap_or_default()).unwrap(),
                        ));
                    }
                }
            }
        }
    }
    None
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        // SDT structure is untracked; the mode does not change behavior.
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("CC".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

// ─── WrapInContentControl ─────────────────────────────────────────────────────

#[test]
fn wrap_plain_text_carries_tag_alias_and_kind() {
    let base = Document::parse(&make_docx("The party agrees to terms")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::WrapInContentControl {
            block_id,
            expect: "party".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: Some("party_field".to_string()),
                alias: Some("Counterparty".to_string()),
                control: SdtControl::PlainText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("apply");

    let (_, _, raw) = first_sdt(&edited.snapshot().canonical).expect("an sdt was created");
    assert!(raw.contains(r#"<w:tag w:val="party_field"/>"#), "raw={raw}");
    assert!(
        raw.contains(r#"<w:alias w:val="Counterparty"/>"#),
        "raw={raw}"
    );
    assert!(raw.contains("<w:text/>"), "raw={raw}");
    // The matched run text is preserved inside sdtContent.
    assert!(raw.contains("party"), "raw={raw}");
}

#[test]
fn wrap_dropdown_lists_items_and_roundtrips() {
    let base = Document::parse(&make_docx("Status is open right now")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::WrapInContentControl {
            block_id,
            expect: "open".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: Some("status".to_string()),
                alias: None,
                control: SdtControl::Dropdown {
                    items: vec![
                        SdtListItem {
                            display: "Open".to_string(),
                            value: "O".to_string(),
                        },
                        SdtListItem {
                            display: "Closed".to_string(),
                            value: "C".to_string(),
                        },
                    ],
                },
                binding: None,
            },
            rationale: None,
        }]))
        .expect("apply");

    let (_, _, raw) = first_sdt(&edited.snapshot().canonical).expect("an sdt was created");
    assert!(raw.contains("<w:dropDownList>"), "raw={raw}");
    assert!(
        raw.contains(r#"w:displayText="Open" w:value="O""#),
        "raw={raw}"
    );
    assert!(
        raw.contains(r#"w:displayText="Closed" w:value="C""#),
        "raw={raw}"
    );

    // Round-trips through serialize → reparse with the Blocking validator.
    let bytes = edited
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize+validate");
    let reparsed = Document::parse(&bytes).expect("reparse");
    let (_, _, raw2) = first_sdt(&reparsed.snapshot().canonical).expect("sdt survives roundtrip");
    assert!(raw2.contains("dropDownList"), "raw2={raw2}");
}

/// SDT structure is untracked: accept-all and reject-all both keep the wrapped
/// control (there is no `w:sdtChange` envelope to resolve).
#[test]
fn wrap_is_untracked_accept_equals_reject_equals_edited() {
    let base = Document::parse(&make_docx("Sign here on the line")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::WrapInContentControl {
            block_id,
            expect: "here".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: Some("sig".to_string()),
                alias: None,
                control: SdtControl::PlainText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("apply");

    let edited_has = first_sdt(&edited.snapshot().canonical).is_some();
    let accepted_has = first_sdt(
        &edited
            .project(Resolution::AcceptAll)
            .expect("accept")
            .snapshot()
            .canonical,
    )
    .is_some();
    let rejected_has = first_sdt(
        &edited
            .project(Resolution::RejectAll)
            .expect("reject")
            .snapshot()
            .canonical,
    )
    .is_some();
    assert!(edited_has, "the edit produced an sdt");
    assert_eq!(
        accepted_has, edited_has,
        "untracked: accept-all keeps the control"
    );
    assert_eq!(
        rejected_has, edited_has,
        "untracked: reject-all keeps the control (no w:sdtChange to undo)"
    );
}

/// Word-free structural mirror of `wrap_content_control_conforms_to_word`
/// (the Word-oracle gold case). Word reads the *serialized* `word/document.xml`,
/// not the IR — so this guards the wrap's untracked-ness at the exact byte layer
/// Word consumes: after BOTH `AcceptAll` and `RejectAll`, the serialized markup
/// must still carry the `w:sdt` (with its `w:sdtContent` and authored `sdtPr`),
/// and must NOT wrap it in any tracked-change envelope (`w:sdtChange`, `w:ins`,
/// `w:del`) — there is no such envelope for an SDT, so accept/reject have nothing
/// to resolve. This is why the tracked `reject-all == original` rule does not
/// apply to this verb.
#[test]
fn wrap_serialized_markup_is_untracked_under_accept_and_reject() {
    fn doc_xml(bytes: &[u8]) -> String {
        let archive = stemma::docx::DocxArchive::read(bytes).expect("read docx");
        String::from_utf8(
            archive
                .get("word/document.xml")
                .expect("document.xml")
                .to_vec(),
        )
        .expect("utf8")
    }
    fn serialize(doc: &Document) -> Vec<u8> {
        doc.serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize+validate")
    }

    let base = Document::parse(&make_docx("The Counterparty shall sign")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::WrapInContentControl {
            block_id,
            expect: "Counterparty".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: Some("party".to_string()),
                alias: Some("Counterparty".to_string()),
                control: SdtControl::PlainText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("apply");

    let accepted = serialize(&edited.project(Resolution::AcceptAll).expect("accept"));
    let rejected = serialize(&edited.project(Resolution::RejectAll).expect("reject"));

    for (label, bytes) in [("accept-all", &accepted), ("reject-all", &rejected)] {
        let xml = doc_xml(bytes);
        // The control persists structurally under either resolution.
        assert!(
            xml.contains("<w:sdt") && xml.contains("<w:sdtContent>"),
            "{label}: the w:sdt content control must survive (untracked structure); xml={xml}"
        );
        assert!(
            xml.contains(r#"<w:tag w:val="party""#)
                && xml.contains(r#"<w:alias w:val="Counterparty""#)
                && (xml.contains("<w:text/>") || xml.contains("<w:text />")),
            "{label}: the authored sdtPr (tag/alias/control kind) must survive; xml={xml}"
        );
        assert!(
            xml.contains(">Counterparty<"),
            "{label}: the wrapped run text must survive inside the sdtContent; xml={xml}"
        );
        // There is no tracked-change envelope around the SDT for Word to resolve.
        assert!(
            !xml.contains("w:sdtChange"),
            "{label}: an SDT has no w:sdtChange envelope — the wrap is untracked; xml={xml}"
        );
        assert!(
            !xml.contains("<w:ins") && !xml.contains("<w:del"),
            "{label}: the wrap must not be emitted as a tracked insertion/deletion; xml={xml}"
        );
    }
}

#[test]
fn wrap_empty_spec_fails_loud() {
    let base = Document::parse(&make_docx("nothing distinguishing here")).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = first_block_id(&canon);
    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::WrapInContentControl {
            block_id,
            expect: "nothing".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: None,
                alias: None,
                control: SdtControl::RichText,
                binding: None,
            },
            rationale: None,
        }]),
    )
    .expect_err("must fail");
    assert!(
        matches!(err, EditError::EmptyContentControlSpec { .. }),
        "got {err:?}"
    );
}

// ─── SetContentControlValue ───────────────────────────────────────────────────

/// Wrap a span in a plain-text control, then set its displayed text.
#[test]
fn set_text_value_updates_content_and_roundtrips() {
    let base = Document::parse(&make_docx("Name: PLACEHOLDER follows")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let wrapped = base
        .apply(&txn(vec![EditStep::WrapInContentControl {
            block_id: block_id.clone(),
            expect: "PLACEHOLDER".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: Some("name".to_string()),
                alias: None,
                control: SdtControl::PlainText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("wrap");

    let (para_id, sdt_id, _) = first_sdt(&wrapped.snapshot().canonical).expect("sdt created");
    let edited = wrapped
        .apply(&txn(vec![EditStep::SetContentControlValue {
            block_id: para_id,
            sdt_id,
            value: SdtValue::Text("Ada Lovelace".to_string()),
            tracked: false,
            rationale: None,
        }]))
        .expect("set value");

    let (_, _, raw) = first_sdt(&edited.snapshot().canonical).expect("sdt present");
    assert!(raw.contains("Ada Lovelace"), "raw={raw}");
    assert!(
        !raw.contains("PLACEHOLDER"),
        "old content replaced: raw={raw}"
    );

    // Round-trips and validates.
    let bytes = edited
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize+validate");
    let reparsed = Document::parse(&bytes).expect("reparse");
    let (_, _, raw2) = first_sdt(&reparsed.snapshot().canonical).expect("sdt survives");
    assert!(raw2.contains("Ada Lovelace"), "raw2={raw2}");
}

/// Wrap a span in a checkbox control, then toggle its checked state.
#[test]
fn set_checkbox_value_toggles_state() {
    let base = Document::parse(&make_docx("Agree X to the terms")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let wrapped = base
        .apply(&txn(vec![EditStep::WrapInContentControl {
            block_id: block_id.clone(),
            expect: "X".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: Some("agree".to_string()),
                alias: None,
                control: SdtControl::Checkbox { checked: false },
                binding: None,
            },
            rationale: None,
        }]))
        .expect("wrap");

    let (para_id, sdt_id, raw0) = first_sdt(&wrapped.snapshot().canonical).expect("sdt created");
    assert!(
        raw0.contains(r#"w14:val="0""#),
        "starts unchecked: raw0={raw0}"
    );

    let edited = wrapped
        .apply(&txn(vec![EditStep::SetContentControlValue {
            block_id: para_id,
            sdt_id,
            value: SdtValue::Checked(true),
            tracked: false,
            rationale: None,
        }]))
        .expect("set checked");
    let (_, _, raw) = first_sdt(&edited.snapshot().canonical).expect("sdt present");
    assert!(raw.contains(r#"w14:val="1""#), "now checked: raw={raw}");
}

#[test]
fn set_value_on_missing_id_fails_not_a_content_control_or_not_found() {
    let base = Document::parse(&make_docx("plain paragraph no controls")).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = first_block_id(&canon);
    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::SetContentControlValue {
            block_id,
            sdt_id: NodeId::from("no_such_sdt"),
            value: SdtValue::Text("x".to_string()),
            tracked: false,
            rationale: None,
        }]),
    )
    .expect_err("must fail");
    assert!(
        matches!(err, EditError::ContentControlNotFound { .. }),
        "got {err:?}"
    );
}

/// Setting a checkbox value on a plain-text control is a type mismatch.
#[test]
fn set_checked_on_plain_text_fails_type_mismatch() {
    let base = Document::parse(&make_docx("Name: FIELD here")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let wrapped = base
        .apply(&txn(vec![EditStep::WrapInContentControl {
            block_id: block_id.clone(),
            expect: "FIELD".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: Some("name".to_string()),
                alias: None,
                control: SdtControl::PlainText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("wrap");
    let (para_id, sdt_id, _) = first_sdt(&wrapped.snapshot().canonical).expect("sdt created");

    let err = apply_transaction(
        &wrapped.snapshot().canonical,
        &txn(vec![EditStep::SetContentControlValue {
            block_id: para_id,
            sdt_id,
            value: SdtValue::Checked(true),
            tracked: false,
            rationale: None,
        }]),
    )
    .expect_err("must fail");
    match err {
        EditError::ContentControlTypeMismatch {
            requested, actual, ..
        } => {
            assert_eq!(requested, "checked");
            assert_eq!(actual, "plain_text");
        }
        other => panic!("got {other:?}"),
    }
}

/// `NotAContentControl`: targeting a non-SDT opaque (a drawing) by id.
#[test]
fn set_value_on_non_sdt_opaque_fails_not_a_content_control() {
    // A paragraph with a drawing opaque and no content control.
    let drawing = r#"<w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:extent cx="1" cy="1"/><wp:docPr id="1" name="P"/></wp:inline></w:drawing></w:r>"#;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p>{drawing}</w:p><w:sectPr/></w:body></w:document>"#
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
    let base = Document::parse(&buf).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = first_block_id(&canon);

    // Find the drawing's opaque id.
    let drawing_id = {
        let mut id = None;
        if let BlockNode::Paragraph(p) = &canon.blocks[0].block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                    {
                        id = Some(o.id.clone());
                    }
                }
            }
        }
        id.expect("a drawing opaque is present")
    };

    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::SetContentControlValue {
            block_id,
            sdt_id: drawing_id,
            value: SdtValue::Text("x".to_string()),
            tracked: false,
            rationale: None,
        }]),
    )
    .expect_err("must fail");
    assert!(
        matches!(err, EditError::NotAContentControl { .. }),
        "got {err:?}"
    );
}

// ─── showingPlcHdr (placeholder state) ────────────────────────────────────────
//
// `w:showingPlcHdr` (a w:sdtPr child, §17.5.2.39) marks a content control as
// displaying *placeholder* text, not a real value. When a value is set, the
// control no longer shows placeholder text — Word would otherwise keep treating
// the new run as placeholder (greyed, discarded on next focus). The setter must
// remove `<w:showingPlcHdr/>` from sdtPr. (This was a latent bug
// in the untracked setter.)

/// A DOCX whose paragraph hosts an INLINE plain-text content control that is in
/// the placeholder state (`<w:showingPlcHdr/>` in sdtPr, placeholder run text).
fn make_docx_with_placeholder_sdt() -> Vec<u8> {
    let sdt = r#"<w:sdt><w:sdtPr><w:rPr/><w:alias w:val="Tenant"/><w:tag w:val="Tenant"/><w:id w:val="123"/><w:showingPlcHdr/><w:text/></w:sdtPr><w:sdtContent><w:r><w:rPr><w:rStyle w:val="PlaceholderText"/></w:rPr><w:t>Click here to enter text.</w:t></w:r></w:sdtContent></w:sdt>"#;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w:body><w:p><w:r><w:t xml:space="preserve">Name: </w:t></w:r>{sdt}</w:p><w:sectPr/></w:body></w:document>"#
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

/// DOMAIN RULE (§17.5.2.39): setting a value on a placeholder-state control must
/// clear `<w:showingPlcHdr/>` — otherwise Word still renders the new text as
/// placeholder. Untracked path (regression guard).
#[test]
fn set_value_clears_showing_placeholder() {
    let base = Document::parse(&make_docx_with_placeholder_sdt()).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, sdt_id, raw_before) = first_sdt(&canon).expect("sdt imported");
    assert!(
        raw_before.contains("showingPlcHdr"),
        "fixture must start in placeholder state: {raw_before}"
    );

    let edited = base
        .apply(&txn(vec![EditStep::SetContentControlValue {
            block_id,
            sdt_id,
            value: SdtValue::Text("Acme Corp".to_string()),
            tracked: false,
            rationale: None,
        }]))
        .expect("set value");

    let (_, _, raw_after) = first_sdt(&edited.snapshot().canonical).expect("sdt present");
    assert!(
        raw_after.contains("Acme Corp"),
        "new value set: {raw_after}"
    );
    assert!(
        !raw_after.contains("showingPlcHdr"),
        "showingPlcHdr must be cleared after setting a value: {raw_after}"
    );
    // The rest of sdtPr (tag/alias) survives — we removed only the placeholder marker.
    assert!(
        raw_after.contains(r#"w:val="Tenant""#),
        "tag/alias preserved: {raw_after}"
    );
}

// ─── Tracked set: refusing stub (part b) ──────────────────────────────────────

/// DOMAIN RULE: a tracked content-control set needs the accept/reject projector
/// to descend into sdtContent revisions (the B1 feature), which is not
/// implemented. `tracked: true` must be REFUSED (no silent downgrade to
/// untracked).
#[test]
fn tracked_set_is_refused_until_b1() {
    let base = Document::parse(&make_docx_with_placeholder_sdt()).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, sdt_id, _) = first_sdt(&canon).expect("sdt imported");
    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::SetContentControlValue {
            block_id,
            sdt_id: sdt_id.clone(),
            value: SdtValue::Text("Acme".to_string()),
            tracked: true,
            rationale: None,
        }]),
    )
    .expect_err("tracked:true must be refused");
    match err {
        EditError::TrackedContentControlSetUnsupported { sdt_id: s, .. } => assert_eq!(s, sdt_id),
        other => panic!("expected TrackedContentControlSetUnsupported, got {other:?}"),
    }
}
