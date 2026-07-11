//! Integration tests for `CreateStyle` / `ModifyStyle` (Verb D) — package-level
//! style-table authoring via the PendingParts `StyleOp` channel.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; §17.7.4):
//!  - CreateStyle adds a `w:style` to `word/styles.xml` (round-trips);
//!  - an `ApplyStyle` referencing the newly-created id validates;
//!  - ModifyStyle replaces a style by id;
//!  - an authored Modify wins a base style-id collision (it runs AFTER the merge);
//!  - fail-loud: Create-existing, Modify-missing (runtime), empty id/name (verb).
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX with a styles part).

use stemma::api::{Document, validate};
use stemma::docx::DocxArchive;
use stemma::domain::{Alignment, NodeId, RevisionInfo};
use stemma::edit::{
    EditError, EditStep, EditTransaction, MaterializationMode, StyleDefinition, StyleParaProps,
    StyleRunProps, StyleType, apply_transaction,
};
use stemma::edit_v4::parse_transaction;
use stemma::runtime::{ErrorCode, ExportOptions};

/// A DOCX with a `word/styles.xml` carrying a single `Normal` style and a
/// paragraph that uses it.
fn make_styled_docx(extra_styles: &str) -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Normal"/></w:pPr><w:r><w:t>A paragraph.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let styles_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>{extra_styles}</w:styles>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

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
        zip.start_file("word/styles.xml", opts).unwrap();
        zip.write_all(styles_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// A minimal DOCX that has **no** `word/styles.xml` part — and correspondingly
/// no styles content-type Override and no `styles` document relationship. This
/// is the shape of a hand-authored or stripped-down real document, and the case
/// that `CreateStyle` must bootstrap a styles part into.
fn make_styleless_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>A heading paragraph.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    // Note: no Override for /word/styles.xml.
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    // Note: no styles relationship.
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

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Styler".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn heading_def(style_id: &str, name: &str) -> StyleDefinition {
    StyleDefinition {
        style_id: style_id.to_string(),
        style_type: StyleType::Para,
        based_on: Some("Normal".to_string()),
        name: name.to_string(),
        run_props: StyleRunProps {
            bold: true,
            font_size_half_points: Some(36),
            ..Default::default()
        },
        para_props: StyleParaProps {
            alignment: Some(Alignment::Center),
            ..Default::default()
        },
    }
}

fn styles_of(bytes: &[u8]) -> String {
    let archive = DocxArchive::read(bytes).expect("read");
    String::from_utf8(archive.get("word/styles.xml").expect("styles").to_vec()).unwrap()
}

// ─── CreateStyle ─────────────────────────────────────────────────────────────

#[test]
fn create_style_adds_w_style_and_validates() {
    let doc = Document::parse(&make_styled_docx("")).unwrap();
    let edited = doc
        .apply(&txn(vec![EditStep::CreateStyle {
            def: heading_def("Heading9", "Heading 9"),
            rationale: None,
        }]))
        .expect("apply CreateStyle");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // The new style round-trips into styles.xml.
    let styles = styles_of(&bytes);
    assert!(
        styles.contains(r#"w:styleId="Heading9""#),
        "new style present: {styles}"
    );
    assert!(
        styles.contains(r#"w:val="Heading 9""#),
        "name present: {styles}"
    );
    assert!(
        styles.contains(r#"w:val="Normal""#),
        "basedOn present: {styles}"
    );
    // The pre-existing Normal style is preserved.
    assert!(
        styles.matches(r#"w:styleId="Normal""#).count() == 1,
        "existing Normal style preserved exactly once"
    );

    // The package opens validator-clean.
    let report = validate(&bytes);
    assert!(
        report.ok,
        "authored-style package must validate: {:?}",
        report.issues
    );
}

#[test]
fn create_then_apply_style_validates() {
    let doc = Document::parse(&make_styled_docx("")).unwrap();
    let block_id = doc.read().blocks[0].id.to_string();

    // Create a style and apply it to the paragraph in one transaction.
    let edited = doc
        .apply(&txn(vec![
            EditStep::CreateStyle {
                def: heading_def("Authored1", "Authored One"),
                rationale: None,
            },
            EditStep::ApplyStyle {
                block_id: NodeId::from(block_id.as_str()),
                semantic_hash: None,
                style_id: "Authored1".to_string(),
                rationale: None,
            },
        ]))
        .expect("apply Create + ApplyStyle");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let styles = styles_of(&bytes);
    assert!(styles.contains(r#"w:styleId="Authored1""#), "style created");

    // The cross-reference (pStyle -> styleId) must be defined: the xref validator
    // would flag a dangling pStyle, so a clean report proves the created style
    // satisfies the ApplyStyle reference.
    let report = validate(&bytes);
    assert!(
        report.ok,
        "ApplyStyle of a freshly-created style must validate (no dangling pStyle): {:?}",
        report.issues
    );
    let doc_xml = {
        let archive = DocxArchive::read(&bytes).expect("read");
        String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap()
    };
    assert!(
        doc_xml.contains(r#"w:val="Authored1""#),
        "paragraph applies the new style"
    );
}

/// REGRESSION (Word-caught): `CreateStyle` into a document that has **no**
/// `word/styles.xml` must bootstrap the styles part rather than failing. The
/// engine already does this for `settings.xml`; styles authoring must follow the
/// same part-bootstrap precedent.
///
/// Before the fix, `apply_pending_style_ops` errored ("word/styles.xml is absent
/// …"), so `create_style_then_apply` in the Word-oracle suite could not even
/// reach Word. This is the Word-free proof of the fix: a real (minimal) document
/// with no styles part gains a valid, registered styles part with the authored
/// style, and an `ApplyStyle` of that new id resolves end to end.
#[test]
fn create_style_bootstraps_absent_styles_part() {
    // Sanity: the fixture genuinely lacks a styles part, rel, and override.
    let base_bytes = make_styleless_docx();
    {
        let archive = DocxArchive::read(&base_bytes).expect("read base");
        assert!(
            archive.get("word/styles.xml").is_none(),
            "fixture must start without a styles part"
        );
        let base_doc_rels = String::from_utf8(
            archive
                .get("word/_rels/document.xml.rels")
                .expect("doc rels")
                .to_vec(),
        )
        .unwrap();
        assert!(
            !base_doc_rels.contains("styles.xml"),
            "fixture must start without a styles relationship"
        );
        let base_ct =
            String::from_utf8(archive.get("[Content_Types].xml").expect("ct").to_vec()).unwrap();
        assert!(
            !base_ct.contains("/word/styles.xml"),
            "fixture must start without a styles content-type override"
        );
    }

    let doc = Document::parse(&base_bytes).unwrap();
    let block_id = doc.read().blocks[0].id.to_string();

    // (a) Create a Para style into the styleless doc AND apply it — both in one
    // transaction, exercising the create_style_then_apply flow end to end.
    let edited = doc
        .apply(&txn(vec![
            EditStep::CreateStyle {
                def: heading_def("AuthoredHeading", "Authored Heading"),
                rationale: None,
            },
            EditStep::ApplyStyle {
                block_id: NodeId::from(block_id.as_str()),
                semantic_hash: None,
                style_id: "AuthoredHeading".to_string(),
                rationale: None,
            },
        ]))
        .expect("CreateStyle + ApplyStyle into a styleless doc must succeed");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // (b) Zero blocking findings from the post-serialization validator.
    let report = validate(&bytes);
    assert!(
        report.ok,
        "bootstrapped-styles package must validate with zero blocking findings: {:?}",
        report.issues
    );

    // (c) The output package now contains the styles part with the authored
    // style, plus the registered relationship and content-type override.
    let archive = DocxArchive::read(&bytes).expect("read output");
    let styles = String::from_utf8(
        archive
            .get("word/styles.xml")
            .expect("styles.xml must now be present")
            .to_vec(),
    )
    .unwrap();
    assert!(
        styles.contains(r#"w:styleId="AuthoredHeading""#),
        "authored style present in synthesized styles.xml: {styles}"
    );

    let doc_rels = String::from_utf8(
        archive
            .get("word/_rels/document.xml.rels")
            .expect("doc rels")
            .to_vec(),
    )
    .unwrap();
    assert!(
        doc_rels
            .contains("http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles")
            && doc_rels.contains(r#"Target="styles.xml""#),
        "a styles relationship must be registered in document.xml.rels: {doc_rels}"
    );

    let content_types =
        String::from_utf8(archive.get("[Content_Types].xml").expect("ct").to_vec()).unwrap();
    assert!(
        content_types.contains(r#"PartName="/word/styles.xml""#)
            && content_types.contains(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"
            ),
        "a styles content-type override must be registered: {content_types}"
    );

    // The applied style id lands on the paragraph (the ApplyStyle resolves).
    let doc_xml = String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        doc_xml.contains(r#"w:val="AuthoredHeading""#),
        "paragraph must apply the newly bootstrapped style: {doc_xml}"
    );

    // (d) The output re-parses (round-trips) and the applied style id resolves.
    let reparsed = Document::parse(&bytes).expect("output must re-parse");
    assert!(
        reparsed.read().blocks.len() == 1,
        "round-trip preserves the single block"
    );
}

// ─── ModifyStyle ─────────────────────────────────────────────────────────────

#[test]
fn modify_style_replaces_by_id() {
    // Start with an existing "Heading1" carrying a plain name.
    let base = make_styled_docx(
        r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="Old Heading"/></w:style>"#,
    );
    let doc = Document::parse(&base).unwrap();
    let edited = doc
        .apply(&txn(vec![EditStep::ModifyStyle {
            style_id: "Heading1".to_string(),
            def: heading_def("Heading1", "New Heading"),
            rationale: None,
        }]))
        .expect("apply ModifyStyle");

    let styles = styles_of(
        &edited
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );
    assert!(
        styles.contains(r#"w:val="New Heading""#),
        "modified name present: {styles}"
    );
    assert!(
        !styles.contains("Old Heading"),
        "old definition replaced: {styles}"
    );
    // Exactly one Heading1 — replaced, not duplicated.
    assert_eq!(
        styles.matches(r#"w:styleId="Heading1""#).count(),
        1,
        "Heading1 replaced exactly once, not duplicated"
    );
}

/// An authored style wins a style-id collision. `apply_pending_style_ops` runs
/// AFTER the base/target style merge, so a `ModifyStyle` overrides whatever the
/// package already had for that id — the contract that makes authored styles
/// authoritative.
#[test]
fn authored_style_wins_id_collision() {
    let base = make_styled_docx(
        r#"<w:style w:type="paragraph" w:styleId="Quote"><w:name w:val="PACKAGE Quote"/></w:style>"#,
    );
    let doc = Document::parse(&base).unwrap();
    let edited = doc
        .apply(&txn(vec![EditStep::ModifyStyle {
            style_id: "Quote".to_string(),
            def: heading_def("Quote", "AUTHORED Quote"),
            rationale: None,
        }]))
        .expect("apply ModifyStyle");

    let styles = styles_of(
        &edited
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );
    assert!(
        styles.contains("AUTHORED Quote"),
        "authored style must win the id collision: {styles}"
    );
    assert!(
        !styles.contains("PACKAGE Quote"),
        "package definition must be replaced, not kept: {styles}"
    );
}

// ─── Fail-loud ───────────────────────────────────────────────────────────────

#[test]
fn create_existing_style_fails_loud() {
    // "Normal" already exists; CreateStyle of it must fail at the save path.
    let doc = Document::parse(&make_styled_docx("")).unwrap();
    let err = match doc.apply(&txn(vec![EditStep::CreateStyle {
        def: heading_def("Normal", "Dup Normal"),
        rationale: None,
    }])) {
        Ok(_) => panic!("Create-existing must fail"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("already exists"),
        "expected create-existing failure, got: {}",
        err.message
    );
}

#[test]
fn modify_missing_style_fails_loud() {
    let doc = Document::parse(&make_styled_docx("")).unwrap();
    let err = match doc.apply(&txn(vec![EditStep::ModifyStyle {
        style_id: "Ghost".to_string(),
        def: heading_def("Ghost", "Ghost"),
        rationale: None,
    }])) {
        Ok(_) => panic!("Modify-missing must fail"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("no style with that id exists"),
        "expected modify-missing failure, got: {}",
        err.message
    );
}

#[test]
fn empty_id_fails_loud_at_verb() {
    let base = Document::parse(&make_styled_docx(""))
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let err = apply_transaction(
        &base,
        &txn(vec![EditStep::CreateStyle {
            def: StyleDefinition {
                style_id: "  ".to_string(),
                style_type: StyleType::Para,
                based_on: None,
                name: "Has Name".to_string(),
                run_props: StyleRunProps::default(),
                para_props: StyleParaProps::default(),
            },
            rationale: None,
        }]),
    )
    .expect_err("empty id must fail");
    assert!(
        matches!(err, EditError::StyleDefEmptyId { .. }),
        "got {err:?}"
    );
}

#[test]
fn empty_name_fails_loud_at_verb() {
    let base = Document::parse(&make_styled_docx(""))
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let err = apply_transaction(
        &base,
        &txn(vec![EditStep::CreateStyle {
            def: StyleDefinition {
                style_id: "HasId".to_string(),
                style_type: StyleType::Para,
                based_on: None,
                name: "".to_string(),
                run_props: StyleRunProps::default(),
                para_props: StyleParaProps::default(),
            },
            rationale: None,
        }]),
    )
    .expect_err("empty name must fail");
    assert!(
        matches!(err, EditError::StyleDefEmptyName { .. }),
        "got {err:?}"
    );
}

/// The Create-existing failure maps to a clear runtime ErrorCode (not a silent
/// InvalidRange fallthrough).
#[test]
fn create_existing_maps_to_unsupported_edit_code() {
    let doc = Document::parse(&make_styled_docx("")).unwrap();
    let err = match doc.apply(&txn(vec![EditStep::CreateStyle {
        def: heading_def("Normal", "Dup"),
        rationale: None,
    }])) {
        Ok(_) => panic!("must fail"),
        Err(e) => e,
    };
    // The runtime maps the styles.xml collision through apply_pending_style_ops →
    // InvalidDocx; the verb-edge errors map to UnsupportedEdit. Either way it is
    // a deliberate, non-default code.
    assert!(
        matches!(
            err.code,
            ErrorCode::InvalidDocx | ErrorCode::UnsupportedEdit
        ),
        "got code {:?}",
        err.code
    );
}

// ─── Wire round-trip ─────────────────────────────────────────────────────────

/// A `modify_style` wire op must deserialize, lower, and apply. The style is
/// addressed by the flattened def's `style_id` (there is no redundant outer
/// `style_id` field); under serde's internally-tagged enum + `#[serde(flatten)]`
/// the outer field used to swallow the `style_id` key and report the def's
/// required `style_id` missing, so every `modify_style` op failed to
/// deserialize. Domain rule: modify = replace-by-id — the targeted style is
/// rewritten in place, never duplicated.
#[test]
fn modify_style_wire_op_round_trips_and_replaces_by_id() {
    let doc = Document::parse(&make_styled_docx("")).unwrap();

    // Rename the pre-existing `Normal` style via the v4 wire format.
    let json = r#"{
        "ops": [
            {
                "op": "modify_style",
                "style_id": "Normal",
                "style_type": "para",
                "name": "Body Text"
            }
        ],
        "revision": { "author": "Styler" }
    }"#;

    let edit_txn = parse_transaction(json)
        .expect("schema check passes")
        .into_edit_transaction()
        .expect("adapter lowers the wire op");

    let edited = doc.apply(&edit_txn).expect("apply modify_style");
    let styles = styles_of(
        &edited
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );

    // Replace-by-id: the new name lands and the style is not duplicated.
    assert!(
        styles.contains(r#"w:val="Body Text""#),
        "modified name present: {styles}"
    );
    assert_eq!(
        styles.matches(r#"w:styleId="Normal""#).count(),
        1,
        "Normal replaced exactly once, not duplicated: {styles}"
    );
}

// ─── SetDocDefaults (the one-edit body-text re-skin) ─────────────────────────

/// `set_doc_defaults` must set the LITERAL docDefaults font + size: body text
/// that inherits from docDefaults picks up the new values without editing any
/// individual style. Domain rule (§17.7.5.3): the new run-font lands as a
/// literal `w:ascii` in `w:docDefaults/w:rPrDefault/w:rPr/w:rFonts`, the size as
/// `w:sz` @val, the projection confirms it is literal (not theme), and the
/// output validates.
#[test]
fn set_doc_defaults_sets_literal_font_and_size() {
    let doc = Document::parse(&make_styled_docx("")).unwrap();
    let edited = doc
        .apply(&txn(vec![EditStep::SetDocDefaults {
            font_family: Some("Georgia".to_string()),
            font_size_half_points: Some(24),
            rationale: None,
        }]))
        .expect("apply SetDocDefaults");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let styles = styles_of(&bytes);
    assert!(
        styles.contains(r#"w:ascii="Georgia""#),
        "docDefaults must carry the literal Georgia font: {styles}"
    );
    assert!(
        styles.contains(r#"w:val="24""#),
        "docDefaults must carry the new size: {styles}"
    );

    // The projection confirms the doc default is now Georgia, LITERAL not theme.
    let proj = edited.snapshot().style_table().expect("project styles");
    assert_eq!(proj.doc_default.font_family.as_deref(), Some("Georgia"));
    assert!(
        !proj.doc_default.font_family_is_theme,
        "Georgia is a literal typeface, not a theme reference"
    );
    assert_eq!(proj.doc_default.font_size_half_points, Some(24));

    // The package opens validator-clean.
    let report = validate(&bytes);
    assert!(
        report.ok,
        "re-skinned package must validate: {:?}",
        report.issues
    );
}

/// `set_doc_defaults` must BOOTSTRAP a styles part when the document has none —
/// the same precedent CreateStyle follows. The docDefaults block is synthesized
/// inside a freshly-registered styles part.
#[test]
fn set_doc_defaults_bootstraps_absent_styles_part() {
    let base_bytes = make_styleless_docx();
    {
        let archive = DocxArchive::read(&base_bytes).expect("read base");
        assert!(
            archive.get("word/styles.xml").is_none(),
            "fixture must start without a styles part"
        );
    }

    let doc = Document::parse(&base_bytes).unwrap();
    let edited = doc
        .apply(&txn(vec![EditStep::SetDocDefaults {
            font_family: Some("Georgia".to_string()),
            font_size_half_points: None,
            rationale: None,
        }]))
        .expect("SetDocDefaults into a styleless doc must succeed");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let styles = styles_of(&bytes);
    assert!(
        styles.contains(r#"w:ascii="Georgia""#),
        "bootstrapped styles.xml must carry the docDefaults font: {styles}"
    );

    let report = validate(&bytes);
    assert!(
        report.ok,
        "bootstrapped-docDefaults package must validate: {:?}",
        report.issues
    );
}

/// Fail-loud: a `set_doc_defaults` wire op that sets NEITHER field is rejected at
/// the schema edge (`parse_transaction`), before any apply.
#[test]
fn set_doc_defaults_empty_fails_loud_at_parse() {
    let json = r#"{
        "ops": [ { "op": "set_doc_defaults" } ],
        "revision": { "author": "Styler" }
    }"#;
    let err = parse_transaction(json).expect_err("empty set_doc_defaults must fail schema check");
    assert!(
        matches!(err, stemma::edit_v4::SchemaError::DocDefaultsEmpty { .. }),
        "got {err:?}"
    );
}

/// The `set_doc_defaults` wire op deserializes, lowers, and applies end to end —
/// proving the v4 adapter wiring (not just the typed EditStep) works.
#[test]
fn set_doc_defaults_wire_op_round_trips() {
    let doc = Document::parse(&make_styled_docx("")).unwrap();
    let json = r#"{
        "ops": [
            { "op": "set_doc_defaults", "font_family": "Georgia", "font_size_half_points": 24 }
        ],
        "revision": { "author": "Styler" }
    }"#;
    let edit_txn = parse_transaction(json)
        .expect("schema check passes")
        .into_edit_transaction()
        .expect("adapter lowers the wire op");
    let edited = doc.apply(&edit_txn).expect("apply set_doc_defaults");
    let proj = edited.snapshot().style_table().expect("project styles");
    assert_eq!(proj.doc_default.font_family.as_deref(), Some("Georgia"));
    assert!(!proj.doc_default.font_family_is_theme);
    assert_eq!(proj.doc_default.font_size_half_points, Some(24));
}
