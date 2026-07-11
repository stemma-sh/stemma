//! SENTINEL — a manually-typed numbering prefix preceded by long leading
//! indentation whitespace is detected regardless of how its runs are split, so
//! the hoist is idempotent across a whole-document rebuild.
//!
//! # The bug this pins
//!
//! `strip_literal_prefix` collects a bounded window of leading text and then
//! looks for a label pattern (`match_prefix_pattern`) that REQUIRES a trailing
//! separator (space/tab) after the label. The window used to be a flat byte cap
//! that counted the paragraph's leading indentation whitespace against its
//! budget. A paragraph can carry arbitrarily long leading whitespace, so when
//! `leading_whitespace + label` filled the window in a single run, the separator
//! — sitting in the NEXT run — fell outside the collected text and the pattern
//! refused to match.
//!
//! That made detection depend on the arbitrary run split. On first import a wild
//! paragraph like `["                     ", " ", "21", ". ", "studenoga 2024."]`
//! (deep manual indent, then a hand-typed `21.` date-list label) hoists fine —
//! the separator sits in a run inside the window. But the whole-document rebuild
//! (the path taken for an untouched paragraph when a DIFFERENT paragraph is
//! edited) coalesces the leading whitespace INTO the label's run, emitting
//! `["                      21.", " ", "studenoga 2024."]`. Re-importing that,
//! the first run alone filled the old byte cap, the separator was invisible, and
//! the prefix was NOT re-hoisted — it stayed in the body. So the SAME paragraph
//! read as a hoisted list label before the rebuild and as plain body text after
//! it: a non-idempotent hoist, which the hermetic fixpoint gate catches as
//! `reject_all(rebuilt) != raw import` on a revision-free original.
//!
//! The fix spends the scan budget only on content PAST the leading whitespace,
//! so the label region — and its separator — is always collected regardless of
//! run boundaries.

use stemma::{
    CanonDoc, DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta, accept_all,
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

fn import(bytes: &[u8]) -> CanonDoc {
    let rt = SimpleRuntime::new();
    std::sync::Arc::unwrap_or_clone(rt.import_docx(bytes).expect("import").canonical)
}

/// FULL canonical text of paragraph `idx`, INCLUDING any hoisted literal prefix
/// (`leading_ws + label + trailing_ws + body`). Mirrors what the bytes carry, so
/// a prefix relocated to the metadata field is still visible to the assertion.
fn paragraph_text_at(doc: &CanonDoc, idx: usize) -> String {
    let p = common::all_paragraphs(doc)[idx];
    let mut out = String::new();
    if let Some(label) = &p.literal_prefix {
        out.push_str(&p.literal_prefix_leading_ws);
        out.push_str(label);
        out.push_str(&p.literal_prefix_trailing_ws);
    }
    out.push_str(&common::paragraph_text(p));
    out
}

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "prefix-ws-scan".to_string(),
        reason: Some("leading-whitespace prefix scan".to_string()),
        timestamp_utc: Some("2026-07-07T00:00:00Z".to_string()),
    }
}

/// Run the runtime redline pipeline (edit a DIFFERENT paragraph, leave the
/// prefix paragraph untouched) and return the exported Redline bytes. This is
/// the whole-document rebuild path that coalesces the untouched paragraph's
/// leading-whitespace and label into a single run.
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

/// The direct pin: the coalesced run structure the rebuild emits. Run 1 is
/// 22 spaces + "21." (25 bytes — enough to fill the old flat byte cap on its
/// own), the separator is a standalone run, then the body. The prefix must still
/// be detected: with the whitespace-inclusive cap it was not.
#[test]
fn coalesced_leading_whitespace_and_label_still_hoists() {
    let para = "<w:p>\
        <w:r><w:t xml:space=\"preserve\">                      21.</w:t></w:r>\
        <w:r><w:t xml:space=\"preserve\"> </w:t></w:r>\
        <w:r><w:t>studenoga 2024.</w:t></w:r></w:p>";
    let doc = import(&pack(&document(para)));
    let p = common::all_paragraphs(&doc)[0];

    assert_eq!(
        p.literal_prefix.as_deref(),
        Some("21."),
        "leading-whitespace-preceded label must be hoisted"
    );
    assert_eq!(p.literal_prefix_leading_ws, "                      ");
    assert_eq!(p.literal_prefix_trailing_ws, " ");
    assert_eq!(common::paragraph_text(p), "studenoga 2024.");
    assert_eq!(
        paragraph_text_at(&doc, 0),
        "                      21. studenoga 2024.",
        "full visible text preserved"
    );
}

/// End-to-end idempotency: a paragraph whose leading runs are split the way Word
/// authored the wild witness (deep indent split across runs, then "21", ". ",
/// body) hoists on import; after the whole-document rebuild reimports it with the
/// whitespace coalesced into the label run, it must STILL be a hoisted prefix and
/// the full text must be identical. Before the fix the rebuilt paragraph lost the
/// hoist and the label leaked into the body (fixpoint violation).
#[test]
fn leading_whitespace_prefix_survives_rebuild() {
    // 21 spaces + 1 space + "21" + ". " + body — the authored split.
    let prefix_para = "<w:p>\
        <w:r><w:t xml:space=\"preserve\">                     </w:t></w:r>\
        <w:r><w:t xml:space=\"preserve\"> </w:t></w:r>\
        <w:r><w:t>21</w:t></w:r>\
        <w:r><w:t xml:space=\"preserve\">. </w:t></w:r>\
        <w:r><w:t>studenoga 2024.</w:t></w:r></w:p>";
    let expected = "                      21. studenoga 2024.";

    let base_body = format!("{prefix_para}<w:p><w:r><w:t>other paragraph</w:t></w:r></w:p>");
    let target_body =
        format!("{prefix_para}<w:p><w:r><w:t>other paragraph EDITED</w:t></w:r></w:p>");
    let a_bytes = pack(&document(&base_body));
    let b_bytes = pack(&document(&target_body));

    // Precondition: the authored split hoists on first import.
    let a_doc = import(&a_bytes);
    assert_eq!(
        common::all_paragraphs(&a_doc)[0].literal_prefix.as_deref(),
        Some("21."),
        "precondition: authored split hoists"
    );
    assert_eq!(paragraph_text_at(&a_doc, 0), expected);

    let exported = redline_export(&a_bytes, &b_bytes);
    let errors: Vec<String> = validate_docx(&exported)
        .errors()
        .map(|f| format!("{f}"))
        .collect();
    assert!(
        errors.is_empty(),
        "redline not validator-clean:\n  {}",
        errors.join("\n  ")
    );

    // FIXPOINT: reparse the rebuilt output. The prefix paragraph must re-hoist
    // (idempotent) and read exactly as before through accept_all / reject_all.
    let mut reparsed = import(&exported);
    assert_eq!(
        common::all_paragraphs(&reparsed)[0]
            .literal_prefix
            .as_deref(),
        Some("21."),
        "FIXPOINT: rebuilt prefix paragraph must re-hoist, not leak the label into the body"
    );
    assert_eq!(
        paragraph_text_at(&reparsed, 0),
        expected,
        "FIXPOINT: full text"
    );

    let mut accepted = reparsed.clone();
    accept_all(&mut accepted);
    assert_eq!(paragraph_text_at(&accepted, 0), expected, "accept_all text");

    reject_all_with_styles(&mut reparsed, None);
    assert_eq!(paragraph_text_at(&reparsed, 0), expected, "reject_all text");
}

/// NON-REGRESSION for the structural-numbering model: a paragraph that carries
/// structural numbering does NOT hoist a leading label-shaped run — the label is
/// rendered by the numbering definition and the leading run is real body text.
/// The wider scan window must not change that: the `numId` paragraph below has a
/// deep leading indent before its baked "5." run, exactly the shape that now
/// scans further, yet the numbering gate must still decline the hoist.
#[test]
fn numbered_paragraph_label_not_hoisted_under_wider_scan() {
    let para = "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr>\
        <w:r><w:t xml:space=\"preserve\">                      5.</w:t></w:r>\
        <w:r><w:t xml:space=\"preserve\"> </w:t></w:r>\
        <w:r><w:t>Numbered body text.</w:t></w:r></w:p>";
    let doc = import(&pack(&document(para)));
    let p = common::all_paragraphs(&doc)[0];

    assert!(
        p.numbering.is_some(),
        "precondition: paragraph carries structural numbering"
    );
    assert_eq!(
        p.literal_prefix, None,
        "a numbering-rendered label must NOT be hoisted, even behind deep leading whitespace"
    );
    // The baked label + body stay in the body verbatim (rendered in addition to
    // the structural number), so the visible text keeps every character.
    assert_eq!(
        common::paragraph_text(p),
        "                      5. Numbered body text.",
    );
}
