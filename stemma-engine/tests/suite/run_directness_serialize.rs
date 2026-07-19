//! Run-formatting fidelity: DIRECT vs INHERITED font/size/color in serialized
//! `<w:rPr>`.
//!
//! Domain rule (ISO 29500-1 §17.7.2 cascade: direct rPr > char style > para
//! style > docDefaults). A run's `TextNode.style_props` holds the fully-RESOLVED
//! *effective* props (the whole cascade collapsed at import). The serializer must
//! NOT bake an *inherited* font/size/color back out as DIRECT `<w:rPr>`: doing so
//! pins the value onto the run, where it shadows any NEW paragraph/character
//! style applied later. The run carries `has_direct_*` provenance precisely so
//! the serializer can tell "authored here" from "inherited" and emit only the
//! former.
//!
//! These invariants are written from the cascade rule, not from current output:
//!
//!  - A: a run that INHERITS its font (no direct rPr) must not gain a direct
//!    `w:rFonts`/`w:sz` after an edit re-serializes its paragraph.
//!  - B: after `apply_style` swaps the paragraph to a style with a DIFFERENT
//!    font, the run's EFFECTIVE rendered font is the NEW style's font (no
//!    baked direct font shadows it).
//!  - C: a run with a genuinely DIRECT (authored) font still serializes it.
//!  - D: `set_format` (SetRunFormatting) setting a run font produces a DIRECT
//!    font in the output (provenance preserved through an edit).
//!
//! The fixture deliberately carries `docDefaults` + a style `rPr` — the shape the
//! older apply-style fixtures lacked, which is why baked-inheritance slipped
//! through.

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::*;
use stemma::edit::*;
use stemma::runtime::ExportOptions;

/// docDefault font is **Cambria 11pt**. `Normal` (para) sets no run props, so a
/// run under it INHERITS Cambria. `FancyBody` (para, based on Normal) sets a run
/// font of **Georgia** — the style-level alternative used to prove the cascade.
///
/// The body has one paragraph (`pStyle=Normal`) with two runs:
///  - run 1 ("Inherited"): no rPr at all  → inherits Cambria (the A/B subject).
///  - run 2 ("Direct"): direct `rFonts ascii="Courier New"` → authored (the C
///    subject).
fn make_cascade_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Normal"/></w:pPr><w:r><w:t>Inherited</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New"/></w:rPr><w:t>Direct</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;

    let styles_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Cambria" w:hAnsi="Cambria"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="FancyBody"><w:name w:val="Fancy Body"/><w:basedOn w:val="Normal"/><w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia"/></w:rPr></w:style></w:styles>"#;

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

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Styler".to_string()),
            date: Some("2026-06-25T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn document_xml_of(bytes: &[u8]) -> String {
    let archive = DocxArchive::read(bytes).expect("read");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .unwrap()
}

fn first_block_id(doc: &Document) -> NodeId {
    NodeId::from(doc.read().blocks[0].id.to_string().as_str())
}

/// The first paragraph's first text run (the INHERITED-font subject).
fn first_text_node(doc: &Document) -> TextNode {
    let canon = &doc.snapshot().canonical;
    let para = match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p,
        other => panic!("first block is not a paragraph: {other:?}"),
    };
    for seg in &para.segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                return (**t).clone();
            }
        }
    }
    panic!("no text run in first paragraph");
}

/// Effective rendered font of the first run, read back from serialized bytes:
/// re-import resolves the cascade into `style_props.font_family`, so this is the
/// font Word would render after the same resolution.
fn effective_first_run_font(bytes: &[u8]) -> Option<String> {
    let reparsed = Document::parse(bytes).expect("reparse serialized output");
    first_text_node(&reparsed)
        .style_props
        .font_family
        .map(|f| f.to_string())
}

/// A neutral structural edit that forces the paragraph through the body
/// serializer: a tracked alignment change (pPrChange). It touches no run props.
fn center_paragraph(doc: &Document) -> Document {
    doc.apply(&txn(vec![EditStep::SetParagraphFormatting {
        block_id: first_block_id(doc),
        semantic_hash: None,
        patch: ParagraphFormattingPatch {
            align: Some(Alignment::Center),
            ..Default::default()
        },
        rationale: None,
    }]))
    .expect("apply alignment change")
}

// ─── Invariant A: inherited stays inherited across an edit ───────────────────

/// A run that inherits its font (no direct rPr) must NOT acquire a direct
/// `w:rFonts`/`w:sz` just because an edit re-serialized its paragraph.
#[test]
fn inherited_font_not_baked_as_direct_after_edit() {
    let doc = Document::parse(&make_cascade_docx()).expect("parse");

    // Precondition: the inherited run genuinely has no direct font provenance.
    let inherited = first_text_node(&doc);
    assert!(
        !inherited.rpr_authored.font_family_any() && !inherited.rpr_authored.font_size,
        "fixture invalid: first run must inherit its font, got {inherited:?}"
    );
    // Its EFFECTIVE font is the docDefault Cambria (cascade resolved at import).
    assert_eq!(
        inherited.style_props.font_family.as_deref(),
        Some("Cambria"),
        "inherited run's effective font is the docDefault"
    );

    let edited = center_paragraph(&doc);
    let xml = document_xml_of(
        &edited
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );

    // The only direct rFonts in the doc belongs to the authored "Courier New"
    // run; the inherited Cambria/size must not be baked onto its run.
    assert_eq!(
        xml.matches(r#"w:ascii="Courier New""#).count(),
        1,
        "the authored direct font survives exactly once: {xml}"
    );
    assert!(
        !xml.contains(r#"w:ascii="Cambria""#),
        "inherited docDefault font must NOT be baked as direct rPr on the run: {xml}"
    );
    assert!(
        !xml.contains(r#"<w:sz w:val="22"/>"#),
        "inherited docDefault size must NOT be baked as direct rPr on the run: {xml}"
    );
}

// ─── Invariant B: a new style's font actually wins ───────────────────────────

/// After `apply_style` swaps the paragraph onto `FancyBody` (Georgia run font),
/// the inherited run's EFFECTIVE rendered font must be Georgia — proving no baked
/// direct Cambria shadows the new style.
#[test]
fn apply_style_new_font_wins_for_inherited_run() {
    let doc = Document::parse(&make_cascade_docx()).expect("parse");

    let edited = doc
        .apply(&txn(vec![EditStep::ApplyStyle {
            block_id: first_block_id(&doc),
            semantic_hash: None,
            style_id: "FancyBody".to_string(),
            rationale: None,
        }]))
        .expect("apply FancyBody style");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // The new style's font is what renders, per the cascade resolved on re-import.
    assert_eq!(
        effective_first_run_font(&bytes).as_deref(),
        Some("Georgia"),
        "the new paragraph style's run font must win; a baked direct Cambria would shadow it"
    );

    // And concretely: no direct Cambria was baked onto the run.
    let xml = document_xml_of(&bytes);
    assert!(
        !xml.contains(r#"w:ascii="Cambria""#),
        "no baked direct docDefault font may appear on the run: {xml}"
    );
}

// ─── Invariant C: a genuinely direct font is preserved ───────────────────────

/// The authored second run carries a DIRECT `Courier New` font. Any edit that
/// re-serializes the paragraph must keep it as direct rPr (no over-suppression).
#[test]
fn authored_direct_font_is_preserved() {
    let doc = Document::parse(&make_cascade_docx()).expect("parse");
    let edited = center_paragraph(&doc);

    let xml = document_xml_of(
        &edited
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );
    assert!(
        xml.contains(r#"w:ascii="Courier New""#),
        "an authored direct font must survive re-serialization: {xml}"
    );
}

// ─── Invariant D: set_format authors a direct font ───────────────────────────

/// `SetRunFormatting` setting a run's font is an authoring action: the resulting
/// run must carry that font as DIRECT rPr (provenance preserved), so it both
/// serializes and renders. We target the inherited run ("Inherited") and give it
/// "Verdana".
#[test]
fn set_format_font_produces_direct_font() {
    let doc = Document::parse(&make_cascade_docx()).expect("parse");

    let edited = doc
        .apply(&txn(vec![EditStep::SetRunFormatting {
            block_id: first_block_id(&doc),
            expect: "Inherited".to_string(),
            semantic_hash: None,
            marks: InlineMarkSet::default(),
            style: RunStyleEdit {
                font_family: Some("Verdana".into()),
                ..Default::default()
            },
            rationale: None,
        }]))
        .expect("apply set run formatting font");

    // Model: the edited run now claims direct font provenance.
    let after = first_text_node(&edited);
    assert!(
        after.rpr_authored.font_family,
        "SetRunFormatting font must set rpr_authored.font_family so the value is authored, not inherited"
    );

    // Output: the direct font is emitted, and re-resolves to Verdana.
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&bytes);
    assert!(
        xml.contains(r#"w:ascii="Verdana""#),
        "the set font must appear as a direct rPr font in the output: {xml}"
    );
    assert_eq!(
        effective_first_run_font(&bytes).as_deref(),
        Some("Verdana"),
        "the directly-set font must render"
    );
}

// ─── Invariant E: literal font / auto color don't gain INHERITED theme attrs ──

/// Per-slot provenance: a run that authors a LITERAL font (`w:ascii`, no
/// `w:asciiTheme`) and an `auto` color (`w:val="auto"`, no `w:themeColor`) must
/// NOT have a theme font or a themeColor injected from the docDefaults cascade on
/// reserialize.
///
/// Domain rule + ECMA §17.3.2.26: when both a literal and a theme attribute are
/// present on `w:rFonts`, the THEME attribute WINS; likewise a `w:themeColor`
/// wins over the literal `w:color w:val`. So injecting an inherited `asciiTheme`
/// onto a run that authored a literal font CHANGES the rendered font, and
/// injecting an inherited `themeColor` onto a run that authored `auto` changes the
/// rendered color (black → the theme's color). The run authored neither theme
/// slot; the serializer must emit neither.
///
/// docDefaults here carry a THEME font (`asciiTheme="minorHAnsi"`) and a
/// `themeColor="text1"`. The body run authors only the literal font + auto color.
fn make_literal_under_theme_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Normal"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Segoe UI Semibold" w:hAnsi="Segoe UI Semibold"/><w:color w:val="auto"/></w:rPr><w:t>Literal</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;

    // docDefaults: a theme font + a themeColor that the cascade resolves INTO the
    // run's effective style_props, but that the run never authored.
    let styles_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi"/><w:color w:val="000000" w:themeColor="text1"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"#;

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

#[test]
fn literal_font_and_auto_color_dont_gain_inherited_theme_attrs() {
    let doc = Document::parse(&make_literal_under_theme_docx()).expect("parse");

    // Precondition (provenance, from the run's OWN rPr): the run authored the
    // literal font slot and the literal/auto color slot, but NOT the theme slots.
    let run = first_text_node(&doc);
    assert!(
        run.rpr_authored.font_family && !run.rpr_authored.font_family_theme,
        "fixture invalid: run must author a literal font, not a theme font: {:?}",
        run.rpr_authored
    );
    assert!(
        run.rpr_authored.color && !run.rpr_authored.color_theme,
        "fixture invalid: run must author a literal/auto color, not a themeColor: {:?}",
        run.rpr_authored
    );

    // Edit elsewhere (paragraph alignment) so the run's paragraph goes through the
    // body serializer without touching the run's rPr.
    let edited = center_paragraph(&doc);
    let xml = document_xml_of(
        &edited
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );

    // The authored literal font + auto color survive.
    assert!(
        xml.contains(r#"w:ascii="Segoe UI Semibold""#),
        "authored literal font must be preserved: {xml}"
    );
    assert!(
        xml.contains(r#"w:val="auto""#),
        "authored auto color must be preserved: {xml}"
    );

    // The inherited theme attrs must NOT be injected onto the run (they would WIN
    // per §17.3.2.26 and change rendering). The body has no other run.
    assert!(
        !xml.contains("asciiTheme"),
        "inherited docDefault theme font must NOT be injected as direct rPr: {xml}"
    );
    assert!(
        !xml.contains("hAnsiTheme"),
        "inherited docDefault theme font must NOT be injected as direct rPr: {xml}"
    );
    assert!(
        !xml.contains("themeColor"),
        "inherited docDefault themeColor must NOT be injected as direct rPr: {xml}"
    );
}
