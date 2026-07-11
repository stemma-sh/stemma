//! SENTINEL — literal numbering-prefix separator survives the whole-document
//! rebuild (diff/redline materialization), not just the verbatim roundtrip.
//!
//! # The bug this pins
//!
//! The importer hoists a literal numbering prefix at paragraph start
//! (`match_prefix_pattern` / `strip_literal_prefix`) into paragraph metadata,
//! capturing the separator whitespace between the label and the body VERBATIM
//! (`literal_prefix_trailing_ws`). The serializer re-emits it via
//! `append_literal_prefix_runs`.
//!
//! When a paragraph like
//!   `<w:r><w:tab/></w:r>×4 <w:r><w:t>28. pluku 1533/29b</w:t></w:r>`
//! (label "28." and body "pluku 1533/29b" separated by ONE space, inside the
//! SAME `w:t`) goes through the whole-document rebuild path — the path taken
//! for UNTOUCHED paragraphs when any *other* paragraph in the document is
//! edited — the separator space after "28." was dropped from the bytes, so Word
//! rendered "28.pluku". Hermetically the corruption shows as a fixpoint
//! violation (#7): reparse of the rebuilt output no longer strips the prefix
//! (the pattern needs the separator), so the text streams diverge.
//!
//! These witnesses drive the redline materialization path (edit a *different*
//! paragraph so the prefix paragraph is untouched) and assert the prefix
//! paragraph's text round-trips EXACTLY through serialize → reparse.

use stemma::{
    CanonDoc, DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta, accept_all,
    docx_validate::validate_docx, reject_all_with_styles,
};

use crate::common;

fn pack(document_xml: &str) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

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

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "prefix-sep".to_string(),
        reason: Some("prefix separator fixpoint".to_string()),
        timestamp_utc: Some("2026-07-05T00:00:00Z".to_string()),
    }
}

/// FULL canonical text of the paragraph at `body`-index `idx`, INCLUDING the
/// hoisted literal prefix: `leading_ws + label + trailing_ws + body`. This is
/// what must round-trip — `common::paragraph_text` alone drops the metadata
/// prefix, so a lost separator would be invisible to it.
fn paragraph_text_at(doc: &CanonDoc, idx: usize) -> String {
    let paras = common::all_paragraphs(doc);
    let p = paras[idx];
    let mut out = String::new();
    if let Some(label) = &p.literal_prefix {
        out.push_str(&p.literal_prefix_leading_ws);
        out.push_str(label);
        out.push_str(&p.literal_prefix_trailing_ws);
    }
    out.push_str(&common::paragraph_text(p));
    out
}

fn import(bytes: &[u8]) -> CanonDoc {
    let rt = SimpleRuntime::new();
    std::sync::Arc::unwrap_or_clone(rt.import_docx(bytes).expect("import").canonical)
}

/// Run the full runtime redline pipeline (edit paragraph B, leave the
/// prefix-bearing paragraph A untouched) and return the exported Redline bytes.
fn redline_export(a_bytes: &[u8], b_bytes: &[u8]) -> Vec<u8> {
    let rt = SimpleRuntime::new();
    let ia = rt.import_docx(a_bytes).expect("import A");
    let ib = rt.import_docx(b_bytes).expect("import B");
    let apply = rt
        .diff_and_redline(&ia.doc_handle, &ib.doc_handle, redline_meta())
        .expect("diff_and_redline");
    assert!(apply.applied, "redline must be applied");
    rt.export_docx(&ia.doc_handle, ExportMode::Redline)
        .expect("export Redline")
}

/// The core witness: paragraph A carries a literal prefix whose separator is a
/// plain space in the SAME run as the body; paragraph B is edited so A is an
/// untouched paragraph rebuilt through the redline materializer. A's text must
/// round-trip exactly through serialize → reparse.
fn assert_prefix_survives_rebuild(prefix_para: &str, expected_a_text: &str) {
    let base_body = format!("{prefix_para}<w:p><w:r><w:t>plain body paragraph</w:t></w:r></w:p>");
    let target_body =
        format!("{prefix_para}<w:p><w:r><w:t>plain body paragraph EDITED</w:t></w:r></w:p>");
    let a_bytes = pack(&document(&base_body));
    let b_bytes = pack(&document(&target_body));

    // Sanity: paragraph A's imported text is the expected full text.
    let a_doc = import(&a_bytes);
    assert_eq!(
        paragraph_text_at(&a_doc, 0),
        expected_a_text,
        "precondition: imported paragraph A text"
    );

    let exported = redline_export(&a_bytes, &b_bytes);

    // The exported bytes must open clean.
    let validation = validate_docx(&exported);
    let errors: Vec<String> = validation.errors().map(|f| format!("{f}")).collect();
    assert!(
        errors.is_empty(),
        "redline output not validator-clean:\n  {}",
        errors.join("\n  ")
    );

    // FIXPOINT (#7): reparse the rebuilt output, accept all, and the untouched
    // prefix paragraph must still read exactly as it did in A — separator space
    // intact, so reparse re-strips the same prefix.
    let mut reparsed = import(&exported);
    let round_a = paragraph_text_at(&reparsed, 0);
    assert_eq!(
        round_a, expected_a_text,
        "FIXPOINT: prefix paragraph text lost through whole-document rebuild"
    );

    // accept_all / reject_all text identity of the untouched paragraph.
    let mut accepted = reparsed.clone();
    accept_all(&mut accepted);
    assert_eq!(
        paragraph_text_at(&accepted, 0),
        expected_a_text,
        "accept_all: untouched prefix paragraph text must survive"
    );
    reject_all_with_styles(&mut reparsed, None);
    assert_eq!(
        paragraph_text_at(&reparsed, 0),
        expected_a_text,
        "reject_all: untouched prefix paragraph text must survive"
    );
}

/// The exact wild-document witness: four tab-only runs, then "28. pluku 1533/29b" with the
/// label and body separated by one space in the same `w:t`.
#[test]
fn prefix_space_separator_survives_rebuild_pluku() {
    let prefix_para = "<w:p>\
        <w:r><w:tab/></w:r><w:r><w:tab/></w:r><w:r><w:tab/></w:r><w:r><w:tab/></w:r>\
        <w:r><w:t>28. pluku 1533/29b</w:t></w:r></w:p>";
    // Canonical text: four tabs + label + space + body.
    assert_prefix_survives_rebuild(prefix_para, "\t\t\t\t28. pluku 1533/29b");
}

/// Space separator with NO leading tabs — the historical default path
/// (`leading_tab_count == 0` used to add the space). Must still survive.
#[test]
fn prefix_space_separator_survives_rebuild_no_leading_tab() {
    let prefix_para = "<w:p><w:r><w:t>1) first item body</w:t></w:r></w:p>";
    assert_prefix_survives_rebuild(prefix_para, "1) first item body");
}

/// Tab separator between the label (in its own run) and the body — the
/// separator lands in `trailing_ws` as a `\t`.
#[test]
fn prefix_tab_separator_survives_rebuild() {
    let prefix_para = "<w:p>\
        <w:r><w:tab/></w:r>\
        <w:r><w:t>1)</w:t></w:r><w:r><w:tab/><w:t>second item body</w:t></w:r></w:p>";
    assert_prefix_survives_rebuild(prefix_para, "\t1)\tsecond item body");
}

/// Parenthesized `(3) ` label with a space separator and leading tabs — same
/// bug class as `28. pluku` (the phone-number false-positive shape from the
/// witness corpus, kept here purely for separator fidelity).
#[test]
fn prefix_paren_space_separator_survives_rebuild() {
    let prefix_para = "<w:p>\
        <w:r><w:tab/></w:r><w:r><w:tab/></w:r>\
        <w:r><w:t>(3) Interpretation clause body</w:t></w:r></w:p>";
    assert_prefix_survives_rebuild(prefix_para, "\t\t(3) Interpretation clause body");
}

/// Bullet label with a space separator and leading tabs.
#[test]
fn prefix_bullet_space_separator_survives_rebuild() {
    let prefix_para = "<w:p>\
        <w:r><w:tab/></w:r>\
        <w:r><w:t>\u{25CF} bullet item body</w:t></w:r></w:p>";
    assert_prefix_survives_rebuild(prefix_para, "\t\u{25CF} bullet item body");
}

/// The one state where import legitimately yields `literal_prefix = Some` with
/// an EMPTY verbatim separator: a parenthesized label whose body starts
/// immediately with no separator (`strip_literal_prefix`'s `explicit_separator
/// == false` branch, which requires leading whitespace + an uppercase/quote
/// body start), e.g. `\t(a)First`. The materializer must NOT invent a separator
/// here — an empty `trailing_ws` is authoritative, not "unknown".
#[test]
fn prefix_paren_no_separator_survives_rebuild_tab_led() {
    // `\t(a)First` — leading tab, label "(a)", no separator, body "First".
    let (leading, label, trailing) =
        import_prefix_fields("<w:p><w:r><w:tab/><w:t>(a)First</w:t></w:r></w:p>");
    assert_eq!(leading, "\t");
    assert_eq!(label, "(a)");
    assert_eq!(
        trailing, "",
        "no-separator paren label captures empty trailing_ws"
    );

    let prefix_para = "<w:p><w:r><w:tab/><w:t>(a)First body</w:t></w:r></w:p>";
    assert_prefix_survives_rebuild(prefix_para, "\t(a)First body");
}

// ── import-capture unit cases: the separator is recorded VERBATIM ──────────

/// Import a single prefix paragraph and return
/// `(leading_ws, label, trailing_ws)`.
fn import_prefix_fields(prefix_para: &str) -> (String, String, String) {
    let bytes = pack(&document(prefix_para));
    let doc = import(&bytes);
    let paras = common::all_paragraphs(&doc);
    let p = paras[0];
    (
        p.literal_prefix_leading_ws.clone(),
        p.literal_prefix.clone().unwrap_or_default(),
        p.literal_prefix_trailing_ws.clone(),
    )
}

#[test]
fn import_captures_space_separator_in_same_run() {
    let (leading, label, trailing) =
        import_prefix_fields("<w:p><w:r><w:t>28. pluku 1533/29b</w:t></w:r></w:p>");
    assert_eq!(leading, "");
    assert_eq!(label, "28.");
    assert_eq!(trailing, " ", "space separator must be captured verbatim");
}

#[test]
fn import_captures_tab_separator() {
    let (_leading, label, trailing) = import_prefix_fields(
        "<w:p><w:r><w:t>1)</w:t></w:r><w:r><w:tab/><w:t>Second</w:t></w:r></w:p>",
    );
    assert_eq!(label, "1)");
    assert_eq!(trailing, "\t", "tab separator must be captured verbatim");
}

#[test]
fn import_captures_paren_space_separator() {
    let (_leading, label, trailing) =
        import_prefix_fields("<w:p><w:r><w:t>(3) Interpretation</w:t></w:r></w:p>");
    assert_eq!(label, "(3)");
    assert_eq!(
        trailing, " ",
        "paren-label space separator captured verbatim"
    );
}
