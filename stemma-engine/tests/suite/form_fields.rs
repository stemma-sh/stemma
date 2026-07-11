//! Integration tests for `SetFormFieldValue` (M2) — filling a legacy form field
//! (FORMTEXT / FORMCHECKBOX / FORMDROPDOWN: the `w:fldChar` + `w:ffData` carrier).
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"):
//!  - FORMTEXT: the result run shows the new text;
//!  - FORMDROPDOWN: ffData `w:result` index AND the result run agree (state⇄render);
//!  - FORMCHECKBOX: ffData `w:checked` flips (no result run);
//!  - refusals: value-not-in-list, fldSimple (NotAFormField), tracked result,
//!    type mismatch, malformed ffData — every refusal has a fixture that fires it.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX). Bytes-in via the public
//! `Document` API.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use stemma::edit::{
    EditError, EditStep, EditTransaction, FormFieldValue, MaterializationMode, apply_transaction,
};
use stemma::{ExportMode, ExportOptions, ValidatorLevel};
use zip::ZipWriter;
use zip::write::FileOptions;

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const PACKAGE_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOC_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(PACKAGE_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(DOC_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn document_xml(doc: &Document) -> String {
    let bytes = doc
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Off,
            validator: None,
        })
        .expect("serialize");
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut xml)
        .expect("read");
    xml
}

/// The (paragraph id, begin-anchor id) of the FIRST form-field BEGIN anchor — a
/// `Field` opaque whose `raw_xml` carries `ffData`. Recurses into table cells.
fn first_form_field(canon: &CanonDoc) -> (NodeId, NodeId) {
    fn in_paragraph(p: &stemma::domain::ParagraphNode) -> Option<(NodeId, NodeId)> {
        for seg in &p.segments {
            for inline in &seg.inlines {
                if let InlineNode::OpaqueInline(o) = inline
                    && matches!(o.kind, OpaqueKind::Field(_))
                    && o.raw_xml
                        .as_deref()
                        .map(|r| String::from_utf8_lossy(r).contains("ffData"))
                        .unwrap_or(false)
                {
                    return Some((p.id.clone(), o.id.clone()));
                }
            }
        }
        None
    }
    fn in_block(block: &BlockNode) -> Option<(NodeId, NodeId)> {
        match block {
            BlockNode::Paragraph(p) => in_paragraph(p),
            BlockNode::Table(t) => t
                .rows
                .iter()
                .flat_map(|r| r.cells.iter())
                .flat_map(|c| c.blocks.iter())
                .find_map(in_block),
            BlockNode::OpaqueBlock(_) => None,
        }
    }
    canon
        .blocks
        .iter()
        .find_map(|tb| in_block(&tb.block))
        .expect("a form-field begin anchor with ffData")
}

fn set_value_txn(block_id: NodeId, field_id: NodeId, value: FormFieldValue) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetFormFieldValue {
            block_id,
            field_id,
            value,
            semantic_hash: None,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("FF".to_string()),
            date: Some("2026-06-11T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

// ─── FORMTEXT ─────────────────────────────────────────────────────────────────

const FORMTEXT_BODY: &str = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="FirstName"/><w:enabled/><w:textInput><w:maxLength w:val="20"/></w:textInput></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>Smith</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;

#[test]
fn set_formtext_replaces_result_run() {
    let base = Document::parse(&make_docx(FORMTEXT_BODY)).expect("parse");
    let (block_id, field_id) = first_form_field(&base.snapshot().canonical);
    let edited = base
        .apply(&set_value_txn(
            block_id,
            field_id,
            FormFieldValue::Text("Jones".to_string()),
        ))
        .expect("set value");
    let xml = document_xml(&edited);
    assert!(
        xml.contains("Jones"),
        "result run shows the new value: {xml}"
    );
    assert!(!xml.contains(">Smith<"), "old value replaced: {xml}");
    // ffData / textInput preserved.
    assert!(
        xml.contains("<w:textInput") && xml.contains(r#"w:val="FirstName""#),
        "ffData preserved: {xml}"
    );
}

// ─── FORMDROPDOWN (state⇄render) ──────────────────────────────────────────────

const FORMDROPDOWN_BODY: &str = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Color"/><w:enabled/><w:ddList><w:result w:val="0"/><w:listEntry w:val="One"/><w:listEntry w:val="Two"/><w:listEntry w:val="Three"/></w:ddList></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMDROPDOWN </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>One</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;

#[test]
fn set_formdropdown_updates_result_index_and_run() {
    let base = Document::parse(&make_docx(FORMDROPDOWN_BODY)).expect("parse");
    let (block_id, field_id) = first_form_field(&base.snapshot().canonical);
    let edited = base
        .apply(&set_value_txn(
            block_id,
            field_id,
            FormFieldValue::Selected("Three".to_string()),
        ))
        .expect("set value");
    let xml = document_xml(&edited);
    // State: ddList/result is the zero-based index of "Three" (= 2).
    assert!(
        xml.contains(r#"<w:result w:val="2""#),
        "ddList result index updated: {xml}"
    );
    // Render: the result run shows "Three".
    assert!(
        xml.contains(">Three<"),
        "result run shows the selected entry: {xml}"
    );
    // The old result run "One" is gone.
    assert!(!xml.contains(">One<"), "old result run replaced: {xml}");
}

#[test]
fn set_formdropdown_value_not_in_list_refuses() {
    let base = Document::parse(&make_docx(FORMDROPDOWN_BODY)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, field_id) = first_form_field(&canon);
    let err = apply_transaction(
        &canon,
        &set_value_txn(
            block_id,
            field_id.clone(),
            FormFieldValue::Selected("Magenta".to_string()),
        ),
    )
    .expect_err("must refuse a value not in the list");
    match err {
        EditError::FormFieldValueNotInList { value, .. } => assert_eq!(value, "Magenta"),
        other => panic!("expected FormFieldValueNotInList, got {other:?}"),
    }
}

// ─── FORMCHECKBOX ─────────────────────────────────────────────────────────────

const FORMCHECKBOX_BODY: &str = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Box1"/><w:enabled/><w:checkBox><w:size w:val="20"/><w:default w:val="0"/><w:checked w:val="false"/></w:checkBox></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMCHECKBOX </w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;

#[test]
fn set_formcheckbox_flips_checked() {
    let base = Document::parse(&make_docx(FORMCHECKBOX_BODY)).expect("parse");
    let (block_id, field_id) = first_form_field(&base.snapshot().canonical);
    let edited = base
        .apply(&set_value_txn(
            block_id,
            field_id,
            FormFieldValue::Checked(true),
        ))
        .expect("set value");
    let xml = document_xml(&edited);
    assert!(
        xml.contains(r#"<w:checked w:val="true""#),
        "w:checked flipped to true: {xml}"
    );
}

#[test]
fn set_text_on_checkbox_refuses_type_mismatch() {
    let base = Document::parse(&make_docx(FORMCHECKBOX_BODY)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, field_id) = first_form_field(&canon);
    let err = apply_transaction(
        &canon,
        &set_value_txn(block_id, field_id, FormFieldValue::Text("x".to_string())),
    )
    .expect_err("text on a checkbox is a type mismatch");
    match err {
        EditError::FormFieldTypeMismatch {
            requested, actual, ..
        } => {
            assert_eq!(requested, "text");
            assert_eq!(actual, "FORMCHECKBOX");
        }
        other => panic!("expected FormFieldTypeMismatch, got {other:?}"),
    }
}

// ─── Multi-run result (§2.3 case 1) ───────────────────────────────────────────

#[test]
fn set_formtext_collapses_multi_run_result() {
    // The cached result spans two runs "Sm" + "ith"; setting a value must
    // replace the WHOLE span with one run, not just the first.
    let body = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="N"/><w:enabled/><w:textInput/></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>Sm</w:t></w:r><w:r><w:t>ith</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let (block_id, field_id) = first_form_field(&base.snapshot().canonical);
    let edited = base
        .apply(&set_value_txn(
            block_id,
            field_id,
            FormFieldValue::Text("X".to_string()),
        ))
        .expect("set value");
    let xml = document_xml(&edited);
    assert!(xml.contains(">X<"), "new single result run: {xml}");
    assert!(
        !xml.contains(">Sm<") && !xml.contains(">ith<"),
        "both old runs replaced: {xml}"
    );
}

// ─── Refusals ─────────────────────────────────────────────────────────────────

#[test]
fn set_on_fldsimple_refuses_not_a_form_field() {
    // A fldSimple " FORMTEXT " is one OpaqueInline{Field, Simple} with no ffData
    // to set — refuse with NotAFormField.
    let body =
        r#"<w:p><w:fldSimple w:instr=" FORMTEXT "><w:r><w:t>Smith</w:t></w:r></w:fldSimple></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    // The fldSimple is the only Field opaque; target it by id directly.
    let field_id = {
        let mut found = None;
        if let BlockNode::Paragraph(p) = &canon.blocks[0].block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Field(_))
                    {
                        found = Some(o.id.clone());
                    }
                }
            }
        }
        found.expect("a fldSimple field opaque")
    };
    let block_id = match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => unreachable!(),
    };
    let err = apply_transaction(
        &canon,
        &set_value_txn(block_id, field_id, FormFieldValue::Text("x".to_string())),
    )
    .expect_err("fldSimple is not a fillable form field");
    assert!(
        matches!(err, EditError::NotAFormField { .. }),
        "got {err:?}"
    );
}

#[test]
fn set_on_missing_id_refuses_not_found() {
    let base = Document::parse(&make_docx(FORMTEXT_BODY)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => unreachable!(),
    };
    let err = apply_transaction(
        &canon,
        &set_value_txn(
            block_id,
            NodeId::from("no_such_field"),
            FormFieldValue::Text("x".to_string()),
        ),
    )
    .expect_err("must fail");
    assert!(
        matches!(err, EditError::FormFieldNotFound { .. }),
        "got {err:?}"
    );
}

#[test]
fn set_formtext_with_tracked_result_refuses() {
    // The cached result run is inside a w:ins — overwriting it would lose the
    // redline; refuse with FormFieldResultHasTrackedChanges.
    let body = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="N"/><w:enabled/><w:textInput/></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:ins w:id="9" w:author="a" w:date="2026-06-11T00:00:00Z"><w:r><w:t>Smith</w:t></w:r></w:ins><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, field_id) = first_form_field(&canon);
    let err = apply_transaction(
        &canon,
        &set_value_txn(block_id, field_id, FormFieldValue::Text("x".to_string())),
    )
    .expect_err("a tracked result must refuse");
    assert!(
        matches!(err, EditError::FormFieldResultHasTrackedChanges { .. }),
        "got {err:?}"
    );
}

#[test]
fn set_on_malformed_ffdata_refuses() {
    // ffData with no textInput/checkBox/ddList child — there is no field-type
    // state to set; refuse with MalformedFfData (don't guess).
    let body = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="X"/><w:enabled/></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>v</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, field_id) = first_form_field(&canon);
    let err = apply_transaction(
        &canon,
        &set_value_txn(block_id, field_id, FormFieldValue::Text("x".to_string())),
    )
    .expect_err("malformed ffData must refuse");
    assert!(
        matches!(err, EditError::MalformedFfData { .. }),
        "got {err:?}"
    );
}

// ─── Nested fields (locator depth pairing) ────────────────────────────────────

#[test]
fn set_outer_field_pairs_correct_end_across_nested_field() {
    // An outer FORMTEXT whose result region contains a NESTED complete field.
    // The locator must pair the OUTER begin with the OUTER end (the last one),
    // not the nested end, so the new value lands in the outer result region.
    let body = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Outer"/><w:enabled/><w:textInput/></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>OuterVal</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    // The OUTER begin is the first Field opaque carrying ffData (the nested PAGE
    // begin has no ffData).
    let (block_id, field_id) = first_form_field(&canon);
    let edited = base
        .apply(&set_value_txn(
            block_id,
            field_id,
            FormFieldValue::Text("NewOuter".to_string()),
        ))
        .expect("set value");
    let xml = document_xml(&edited);
    assert!(xml.contains("NewOuter"), "outer value set: {xml}");

    // DECISION (documented Word-like semantic, see apply_set_value): a NESTED
    // field that lives wholly inside the result region is part of the cached
    // result, so the whole-result-span replace drops it WHOLE (its begin AND end
    // both go). The nested PAGE field is gone...
    assert!(!xml.contains("PAGE"), "nested field dropped whole: {xml}");
    // ...and the markup stays BALANCED: exactly the outer field's three fldChars
    // survive (begin/separate/end), no orphaned nested begin/end.
    let begins = xml.matches(r#"w:fldCharType="begin""#).count();
    let separates = xml.matches(r#"w:fldCharType="separate""#).count();
    let ends = xml.matches(r#"w:fldCharType="end""#).count();
    assert_eq!(
        (begins, separates, ends),
        (1, 1, 1),
        "only the outer field's balanced fldChars remain (no orphan from the dropped nested field): {xml}"
    );
}

// ─── Table cell (§2.3 case 4) ─────────────────────────────────────────────────

#[test]
fn set_formtext_in_table_cell() {
    let body = format!(r#"<w:tbl><w:tr><w:tc><w:tcPr/>{FORMTEXT_BODY}</w:tc></w:tr></w:tbl>"#);
    let base = Document::parse(&make_docx(&body)).expect("parse");
    let (block_id, field_id) = first_form_field(&base.snapshot().canonical);
    let edited = base
        .apply(&set_value_txn(
            block_id,
            field_id,
            FormFieldValue::Text("InCell".to_string()),
        ))
        .expect("set value in a table cell");
    let xml = document_xml(&edited);
    assert!(xml.contains("InCell"), "value set in-cell: {xml}");
}
