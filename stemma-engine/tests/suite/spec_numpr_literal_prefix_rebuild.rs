//! SENTINEL — a numbered paragraph that ALSO carries a literal baked-in label
//! run keeps that run's visible text through the whole-document rebuild
//! (diff/redline materialization) and through import→export cycles.
//!
//! # The bug this pins
//!
//! Wild Word-authored (often converted/generated) documents contain paragraphs
//! that have BOTH structural numbering (`<w:numPr>`) AND a literal baked-in
//! label run, e.g. runs `["10. ", "J. MARTINS"]` under a `numId`. On import the
//! literal-prefix heuristic (`match_prefix_pattern` / `strip_literal_prefix`)
//! used to hoist `"10. "` into paragraph metadata even for these numbered
//! paragraphs. The serializer then suppressed the hoisted prefix on the theory
//! that Word regenerates the label from the numbering definition — so the
//! visible text `"10. "` vanished from the exported bytes on ANY rebuild (a
//! redline export where a DIFFERENT paragraph was edited). Word accept AND
//! reject of such a redline lost the text.
//!
//! Worse, the loss cascaded: reimporting the (already stripped) output hoisted
//! the NEXT prefix-shaped token — `"J. "` (a person's initial) — so each
//! import→export cycle destroyed more text
//! (`"10. J. MARTINS"` → `"J. MARTINS"` → `"MARTINS"`).
//!
//! The fix: the literal-prefix heuristic does not hoist when the paragraph has
//! effective structural numbering. The label there is rendered by numbering; a
//! leading label-shaped run is real body text, not a hoistable presentational
//! prefix. It therefore stays in the body verbatim and round-trips as bytes.

use stemma::api::Document;
use stemma::{
    CanonDoc, DocxRuntime, ExportMode, ExportOptions, SimpleRuntime, TransactionMeta, accept_all,
    docx_validate::validate_docx, reject_all_with_styles,
};

use crate::common;

const NUMBERING_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;

fn pack(document_xml: &str) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(NUMBERING_XML.as_bytes()).unwrap();
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
        author: "numpr-prefix".to_string(),
        reason: Some("numPr literal prefix rebuild".to_string()),
        timestamp_utc: Some("2026-07-06T00:00:00Z".to_string()),
    }
}

fn import(bytes: &[u8]) -> CanonDoc {
    let rt = SimpleRuntime::new();
    std::sync::Arc::unwrap_or_clone(rt.import_docx(bytes).expect("import").canonical)
}

/// FULL canonical text of paragraph `idx`, INCLUDING any hoisted literal prefix
/// (`leading_ws + label + trailing_ws + body`). Mirrors what the bytes carry, so
/// a prefix lost to the metadata field is still visible to the assertion.
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

/// Run the runtime redline pipeline (edit a DIFFERENT paragraph, leave the
/// numbered prefix paragraph untouched) and return the exported Redline bytes.
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

/// A numbered paragraph (numId=1) whose runs are `["10. ", "J. MARTINS"]` — real
/// structural numbering plus a baked-in literal label.
const NUMBERED_PREFIX_PARA: &str = "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr><w:r><w:t xml:space=\"preserve\">10. </w:t></w:r><w:r><w:t>J. MARTINS</w:t></w:r></w:p>";

const NUMBERED_PREFIX_TEXT: &str = "10. J. MARTINS";

#[test]
fn numbered_paragraph_literal_prefix_not_hoisted() {
    let bytes = pack(&document(NUMBERED_PREFIX_PARA));
    let doc = import(&bytes);
    let p = common::all_paragraphs(&doc)[0];

    assert!(
        p.numbering.is_some(),
        "precondition: paragraph must carry structural numbering (numId=1)"
    );
    assert!(
        p.literal_prefix.is_none(),
        "a numbered paragraph's leading label run is body text, not a hoisted \
         literal prefix; got literal_prefix = {:?}",
        p.literal_prefix
    );
    assert_eq!(
        common::paragraph_text(p),
        NUMBERED_PREFIX_TEXT,
        "the '10. ' run must remain in the body verbatim"
    );
}

#[test]
fn numbered_prefix_survives_rebuild() {
    let base_body =
        format!("{NUMBERED_PREFIX_PARA}<w:p><w:r><w:t>plain body paragraph</w:t></w:r></w:p>");
    let target_body = format!(
        "{NUMBERED_PREFIX_PARA}<w:p><w:r><w:t>plain body paragraph EDITED</w:t></w:r></w:p>"
    );
    let a_bytes = pack(&document(&base_body));
    let b_bytes = pack(&document(&target_body));

    let a_doc = import(&a_bytes);
    assert_eq!(
        paragraph_text_at(&a_doc, 0),
        NUMBERED_PREFIX_TEXT,
        "precondition: imported numbered paragraph text"
    );

    let exported = redline_export(&a_bytes, &b_bytes);

    let validation = validate_docx(&exported);
    let errors: Vec<String> = validation.errors().map(|f| format!("{f}")).collect();
    assert!(
        errors.is_empty(),
        "redline output not validator-clean:\n  {}",
        errors.join("\n  ")
    );

    let mut reparsed = import(&exported);
    assert_eq!(
        paragraph_text_at(&reparsed, 0),
        NUMBERED_PREFIX_TEXT,
        "rebuild dropped the numbered paragraph's literal label run"
    );

    let mut accepted = reparsed.clone();
    accept_all(&mut accepted);
    assert_eq!(
        paragraph_text_at(&accepted, 0),
        NUMBERED_PREFIX_TEXT,
        "accept_all: numbered paragraph label text must survive"
    );
    reject_all_with_styles(&mut reparsed, None);
    assert_eq!(
        paragraph_text_at(&reparsed, 0),
        NUMBERED_PREFIX_TEXT,
        "reject_all: numbered paragraph label text must survive"
    );
}

/// The cascade guard: import→serialize→import must be text-stable. A dropped or
/// re-hoisted prefix would erode more text on each cycle
/// (`"10. J. MARTINS"` → `"J. MARTINS"` → `"MARTINS"`).
#[test]
fn numbered_prefix_import_export_idempotent() {
    let bytes = pack(&document(NUMBERED_PREFIX_PARA));
    let doc = import(&bytes);

    // Two full cycles: if any cycle hoisted "J." out of the body, the third
    // paragraph text would read "J. MARTINS" (or "MARTINS"), not the original.
    let mut current = bytes.clone();
    for cycle in 0..2 {
        let parsed = Document::parse(&current).expect("parse");
        let exported = parsed
            .serialize(&ExportOptions::default())
            .expect("serialize");
        let reparsed = import(&exported);
        assert_eq!(
            paragraph_text_at(&reparsed, 0),
            paragraph_text_at(&doc, 0),
            "cycle {cycle}: import(serialize(import(A))) must be text-stable (cascade guard)"
        );
        assert_eq!(paragraph_text_at(&reparsed, 0), NUMBERED_PREFIX_TEXT);
        // The "10. " run is body text under numPr, never hoisted — so no
        // literal-prefix field is populated and the next token ("J. ") is safe.
        let p = common::all_paragraphs(&reparsed)[0];
        assert!(
            p.literal_prefix.is_none(),
            "cycle {cycle}: numbered paragraph must not hoist any literal prefix"
        );
        current = exported;
    }
}

/// Regression guard for the 472ee8e behavior: a paragraph with NO structural
/// numbering still hoists its literal label prefix and round-trips it verbatim
/// through the rebuild — even when the document carries a numbering part. The
/// structural-numbering gate must key off THIS paragraph's numPr, not the mere
/// presence of numbering definitions.
#[test]
fn non_numbered_prefix_still_hoisted_and_survives_rebuild() {
    // A plain paragraph (no numPr) whose text begins with a literal "1) " label.
    let prefix_para = "<w:p><w:r><w:t>1) first item body</w:t></w:r></w:p>";
    let expected = "1) first item body";

    let plain = import(&pack(&document(prefix_para)));
    let p = common::all_paragraphs(&plain)[0];
    assert!(
        p.numbering.is_none(),
        "precondition: paragraph has no structural numbering"
    );
    assert_eq!(
        p.literal_prefix.as_deref(),
        Some("1)"),
        "non-numbered paragraph must still hoist its literal prefix"
    );

    let base_body = format!("{prefix_para}<w:p><w:r><w:t>plain body paragraph</w:t></w:r></w:p>");
    let target_body =
        format!("{prefix_para}<w:p><w:r><w:t>plain body paragraph EDITED</w:t></w:r></w:p>");
    let exported = redline_export(&pack(&document(&base_body)), &pack(&document(&target_body)));

    let mut reparsed = import(&exported);
    assert_eq!(
        paragraph_text_at(&reparsed, 0),
        expected,
        "non-numbered literal prefix lost through rebuild (472ee8e regression)"
    );
    reject_all_with_styles(&mut reparsed, None);
    assert_eq!(
        paragraph_text_at(&reparsed, 0),
        expected,
        "reject_all: non-numbered literal prefix must survive"
    );
}
