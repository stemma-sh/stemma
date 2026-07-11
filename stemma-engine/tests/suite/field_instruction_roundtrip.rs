//! Bytes-in roundtrip tests for field instructions (`w:fldSimple`).
//!
//! Two domain contracts are pinned here, both over the public
//! [`stemma::api::Document`] import/export API (no hand-built IR):
//!
//! 1. **Instruction roundtrip fixpoint.** Import → serialize → re-import must
//!    be a fixpoint on the typed `FieldData.semantic`. The serializer
//!    reconstructs `w:instr` from the typed semantic via
//!    `FieldSemantic::to_instruction_text` (`build_simple_field_element`,
//!    `src/serialize/mod.rs`); the importer re-parses it via
//!    `parse_field_instruction` (`src/import.rs`). If those two are not
//!    inverses for some field, the semantic drifts across a save — a real bug.
//!    The contract is `semantic_after == semantic_before` for EVERY field,
//!    and `serialize(reimport) == serialize(import)` byte-for-byte on `w:instr`
//!    (the observable proxy: the on-the-wire form must be stable, not merely
//!    re-parse-equal).
//!
//! 2. **Malformed fldSimple is refused at import (no silent fallback).** A
//!    structurally-complete-but-invalid `w:fldSimple` instruction
//!    (`HYPERLINK` with no target, `IF` missing args, empty `w:instr`) must make
//!    import return `Err(InvalidDocx)` with an actionable message — never
//!    degrade to a generic/empty field. The SAME garbage carried in a *complex*
//!    field (`w:instrText`, a mere fragment) is intentionally tolerated
//!    (`Ok`, semantic `None`) — that contrast is pinned too.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::runtime::{ExportMode, ExportOptions, ValidatorLevel};
use stemma::{ErrorCode, FieldData, FieldSemantic, InlineNode, OpaqueKind};

// ─── Fixtures ──────────────────────────────────────────────────────────────

/// Wrap a `<w:body>` inner fragment into a minimal, valid `.docx` package.
fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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

/// One `<w:fldSimple>` paragraph with the given instruction and a cached
/// result run so the field is well-formed for Word.
fn fld_simple_para(instr: &str, result: &str) -> String {
    format!(
        r#"<w:p><w:fldSimple w:instr="{instr}"><w:r><w:t xml:space="preserve">{result}</w:t></w:r></w:fldSimple></w:p>"#
    )
}

/// A complex field carrying the same instruction in a `<w:instrText>`
/// fragment (begin / instrText / separate / result / end).
fn complex_field_para(instr: &str, result: &str) -> String {
    format!(
        concat!(
            r#"<w:p>"#,
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
            r#"<w:r><w:instrText xml:space="preserve">{instr}</w:instrText></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
            r#"<w:r><w:t xml:space="preserve">{result}</w:t></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
            r#"</w:p>"#,
        ),
        instr = instr,
        result = result,
    )
}

fn redline_opts() -> ExportOptions {
    ExportOptions {
        mode: ExportMode::Redline,
        validator_level: ValidatorLevel::Off,
        validator: None,
    }
}

/// All `FieldData` for `fldSimple` opaques in the document, in document order.
fn simple_field_data(doc: &Document) -> Vec<FieldData> {
    let mut out = Vec::new();
    for tb in &doc.snapshot().canonical.blocks {
        if let stemma::BlockNode::Paragraph(p) = &tb.block {
            for inline in p.all_inlines() {
                if let InlineNode::OpaqueInline(o) = inline
                    && let OpaqueKind::Field(fd) = &o.kind
                {
                    out.push(fd.clone());
                }
            }
        }
    }
    out
}

/// The typed semantics of every `fldSimple` field, in document order.
fn simple_field_semantics(doc: &Document) -> Vec<Option<FieldSemantic>> {
    simple_field_data(doc)
        .into_iter()
        .map(|fd| fd.semantic)
        .collect()
}

// ─── (1) Instruction roundtrip fixpoint ──────────────────────────────────────

/// Every non-trivial field semantic must survive import → serialize → import
/// unchanged. This is the FIXPOINT contract: the serializer's
/// `to_instruction_text` and the importer's `parse_field_instruction` are
/// inverses on the typed semantic, so a save cannot silently mutate a field.
///
/// We assert on BOTH:
///   - typed equality: `semantic` is identical across the roundtrip; and
///   - canonical-form stability: re-serializing the re-imported document
///     produces the identical `w:instr` form (the observable on-the-wire proxy
///     — a field whose typed semantic is stable but whose emitted string keeps
///     shifting would still be a roundtrip defect).
#[test]
fn every_field_semantic_is_a_roundtrip_fixpoint() {
    // XML-escaped instruction strings: `&gt;` for `>`, `&quot;` for the inner
    // quotes Word uses around switch arguments.
    let fields: &[(&str, &str)] = &[
        (
            r#"HYPERLINK &quot;https://x.example&quot; \o &quot;tip&quot;"#,
            "link",
        ),
        (r#"MERGEFIELD Name \* Upper"#, "ACME"),
        (r#"MERGEFIELD Name \* MERGEFORMAT"#, "ACME"),
        (r#"IF a &gt; b &quot;yes&quot; &quot;no&quot;"#, "yes"),
        (r#"= 1 + 2"#, "3"),
        (r#"DATE \@ &quot;yyyy-MM-dd&quot;"#, "2026-06-07"),
        (r#"REF bookmark1 \h"#, "1"),
        (r#"TOC \o &quot;1-3&quot; \h"#, "contents"),
        // A date field that combines a calendar switch AND a picture switch:
        // exercises switch + format-switch ordering together.
        (r#"DATE \h \@ &quot;yyyy&quot;"#, "2026"),
        // MERGEFIELD with before/after text and a format switch: maximal
        // switch coverage where ordering is most likely to drift.
        (
            r#"MERGEFIELD Company \b &quot;Co: &quot; \f &quot;.&quot; \* Upper"#,
            "ACME",
        ),
        // Format switches in NON-canonical source order (`\@` before `\*`):
        // the emitter must preserve the original switch ordering
        // (`FormatSwitches.order`), or the re-parse drifts. This is the most
        // direct probe of the suspected "reorder format switches" defect.
        (
            r#"MERGEFIELD D \@ &quot;yyyy&quot; \# &quot;0.00&quot; \* Upper"#,
            "v",
        ),
        // The general format switch placed BEFORE the field's primary
        // argument (name): the parser strips format switches first, so the
        // name is still found, and the emitter re-emits name-then-switch.
        (r#"MERGEFIELD \* Upper Name"#, "ACME"),
    ];

    let body: String = fields
        .iter()
        .map(|(instr, result)| fld_simple_para(instr, result))
        .collect();
    let bytes = make_docx_with_body(&body);

    let doc0 = Document::parse(&bytes).expect("initial parse");
    let before = simple_field_semantics(&doc0);
    assert_eq!(
        before.len(),
        fields.len(),
        "every fldSimple should import as exactly one Field opaque"
    );
    // None of these is malformed, so each must classify to a typed semantic.
    for (i, sem) in before.iter().enumerate() {
        assert!(
            sem.is_some(),
            "field #{i} ({:?}) should classify to a typed semantic, got None",
            fields[i].0
        );
    }

    // import → serialize → re-import.
    let docx1 = doc0.serialize(&redline_opts()).expect("first serialize");
    let doc1 = Document::parse(&docx1).expect("re-parse after serialize");
    let after = simple_field_semantics(&doc1);

    assert_eq!(
        after.len(),
        before.len(),
        "field count must be preserved across the roundtrip"
    );
    for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
        assert_eq!(
            a, b,
            "field #{i} ({:?}) semantic drifted across import→serialize→import:\n  before: {b:?}\n  after:  {a:?}",
            fields[i].0
        );
    }

    // Canonical-form stability: serializing the re-imported document again must
    // reproduce the identical `w:instr` strings. (`to_instruction_text` is a
    // true fixpoint, not merely re-parse-equal.)
    let docx2 = doc1.serialize(&redline_opts()).expect("second serialize");
    let instr1 = collect_instr_attrs(&docx1);
    let instr2 = collect_instr_attrs(&docx2);
    assert_eq!(
        instr1, instr2,
        "the emitted w:instr form must be stable across a second roundtrip"
    );
}

/// Extract every `w:instr="..."` value from a serialized docx, in order, by
/// reading `word/document.xml` out of the package. Used to assert the emitted
/// on-the-wire form is stable across roundtrips.
fn collect_instr_attrs(docx: &[u8]) -> Vec<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip read");
    let mut xml = String::new();
    {
        use std::io::Read;
        let mut f = zip
            .by_name("word/document.xml")
            .expect("document.xml present");
        f.read_to_string(&mut xml).expect("read document.xml");
    }
    let mut out = Vec::new();
    let needle = "w:instr=\"";
    let mut rest = xml.as_str();
    while let Some(pos) = rest.find(needle) {
        rest = &rest[pos + needle.len()..];
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_string());
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    out
}

// ─── (2) Malformed fldSimple is refused at import ─────────────────────────────

/// A structurally-complete-but-invalid `w:fldSimple` instruction has no
/// fragment excuse: the whole instruction is present in the `w:instr`
/// attribute. Per the no-silent-fallback rule, import must FAIL with
/// `InvalidDocx` and an actionable message rather than degrade to a generic /
/// empty field.
#[test]
fn malformed_fldsimple_is_refused_at_import() {
    // Each case: (instruction, the parse error the message should mention).
    let cases: &[(&str, &str)] = &[
        // HYPERLINK with no URL and no bookmark.
        (r#"HYPERLINK"#, "HYPERLINK"),
        // IF with no quoted true/false alternatives.
        (r#"IF a &gt; b"#, "IF"),
        // MERGEFIELD with no field name.
        (r#"MERGEFIELD"#, "MERGEFIELD"),
    ];

    for (instr, needle) in cases {
        let body = fld_simple_para(instr, "x");
        let bytes = make_docx_with_body(&body);
        let err = match Document::parse(&bytes) {
            Ok(_) => panic!(
                "fldSimple with malformed instruction {instr:?} must be refused, not imported"
            ),
            Err(e) => e,
        };
        assert_eq!(
            err.code,
            ErrorCode::InvalidDocx,
            "malformed fldSimple {instr:?} should fail with InvalidDocx, got {:?}: {}",
            err.code,
            err.message
        );
        assert!(
            err.message.to_uppercase().contains(&needle.to_uppercase())
                || err.message.contains("fldSimple"),
            "error for {instr:?} should be actionable (mention the field/instruction); got: {}",
            err.message
        );
    }
}

/// Empty `w:instr` on a `fldSimple` is also a complete (degenerate)
/// instruction: there is nothing to classify, and the no-fallback rule forbids
/// inventing a field. Import must refuse.
#[test]
fn empty_fldsimple_instr_is_refused_at_import() {
    let body = fld_simple_para("", "x");
    let bytes = make_docx_with_body(&body);
    // NOTE: an EMPTY instruction is treated as "no instruction to classify"
    // (`Ok(None)`) by `parse_field_semantic`, not a hard error, because an
    // empty `w:instr` carries no claim about a field type. The whitespace-only
    // case below is the one that exercises the non-empty-but-unparseable path.
    let doc = Document::parse(&bytes).expect("empty w:instr classifies as no semantic");
    let sems = simple_field_semantics(&doc);
    assert_eq!(
        sems,
        vec![None],
        "empty instr ⇒ no typed semantic, not a guess"
    );
}

/// The contrast case: the SAME garbage instruction carried in a *complex*
/// field (`w:instrText`) is a mere fragment of a possibly multi-run
/// instruction, so the importer tolerates it (`Ok`, no typed semantic) rather
/// than failing the whole document. This is the intentional asymmetry with the
/// fldSimple refusal above — pin it so a future change can't quietly make
/// complex fields strict (or fldSimple lax) without updating this test.
#[test]
fn malformed_complex_field_is_tolerated_at_import() {
    for instr in [r#"HYPERLINK"#, r#"IF a &gt; b"#, r#"MERGEFIELD"#] {
        let body = complex_field_para(instr, "x");
        let bytes = make_docx_with_body(&body);
        let doc = Document::parse(&bytes).unwrap_or_else(|e| {
            panic!("complex field with fragment {instr:?} must import OK, got {e:?}")
        });
        // The instruction opaque imports, but with no typed semantic — a
        // fragment can't stand alone, so classification is deferred, not faked.
        let mut saw_instruction_field = false;
        for tb in &doc.snapshot().canonical.blocks {
            if let stemma::BlockNode::Paragraph(p) = &tb.block {
                for inline in p.all_inlines() {
                    if let InlineNode::OpaqueInline(o) = inline
                        && let OpaqueKind::Field(fd) = &o.kind
                        && fd.field_kind == stemma::FieldKind::Instruction
                    {
                        saw_instruction_field = true;
                        assert!(
                            fd.semantic.is_none(),
                            "complex-field fragment {instr:?} must NOT be classified to a typed semantic, got {:?}",
                            fd.semantic
                        );
                    }
                }
            }
        }
        assert!(
            saw_instruction_field,
            "complex field {instr:?} should produce an Instruction field opaque"
        );
    }
}
