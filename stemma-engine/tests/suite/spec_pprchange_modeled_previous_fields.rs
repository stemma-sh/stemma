//! Model-level reject fidelity for the pPrChange previous-pPr MODELED fields.
//!
//! `w:pPrChange` (§17.13.5.29) requires the inner w:pPr to be a COMPLETE
//! snapshot of the previous paragraph properties, and reject must restore the
//! prior state EXACTLY (`reject_paragraph_formatting`'s reversibility
//! contract). The domain's `ParagraphFormattingChange` models ~20 previous_*
//! fields (style, keepNext, borders, shading, tabs, …), but import used to
//! hardcode all of them to "absent" — only jc/ind/spacing/rPr were parsed
//! from the snapshot. The preserved-remainder bag masked this at the XML
//! level (raw children round-trip verbatim), but the in-memory model after
//! reject LIED: `p.keep_next == None` while the pPr remainder carried a
//! verbatim `<w:keepNext/>`. Any model consumer (read_format, style
//! resolution, a later edit to the same property) then saw or produced wrong
//! state — a later `keep_next` edit would double-write the element.
//!
//! This file pins the fix: every inner-pPr child with a previous_* domain
//! field is parsed into that field (and thus restored as MODEL state on
//! reject), and only genuinely unmodeled children (w:suppressLineNumbers,
//! w:numPr, …) remain in the preserved bag.
//!
//! Uses a hermetic in-memory `.docx` (no corpus fixtures) so this runs daily.

use std::io::{Cursor, Read, Write};

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::*;
use stemma::runtime::Resolution;
use zip::ZipWriter;
use zip::write::FileOptions;

// ── DOCX builder helpers (same minimal-package style as
//    spec_pprchange_preserved_remainder.rs) ────────────────────────────────

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const WORD_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

fn build_docx(document_xml: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(WORD_RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();

    let cursor = zip.finish().unwrap();
    cursor.into_inner()
}

/// A paragraph with a pre-existing `w:pPrChange` (as Word writes it) whose
/// previous-pPr snapshot exercises EVERY inner-pPr child that has a
/// `previous_*` field on `ParagraphFormattingChange`, plus two that do NOT
/// (w:suppressLineNumbers, w:numPr) and must therefore stay in the preserved
/// remainder. Children appear in CT_PPrBase schema order, as Word emits them.
fn snapshot_bearing_body() -> &'static str {
    r#"
    <w:p>
      <w:pPr>
        <w:jc w:val="center"/>
        <w:pPrChange w:id="77" w:author="Word User" w:date="2026-06-01T00:00:00Z">
          <w:pPr>
            <w:pStyle w:val="Quote"/>
            <w:keepNext/>
            <w:keepLines w:val="0"/>
            <w:pageBreakBefore/>
            <w:framePr w:w="2000" w:h="400" w:wrap="around" w:vAnchor="text"/>
            <w:widowControl w:val="0"/>
            <w:numPr>
              <w:ilvl w:val="0"/>
              <w:numId w:val="5"/>
            </w:numPr>
            <w:suppressLineNumbers/>
            <w:pBdr>
              <w:top w:val="single" w:sz="4" w:space="1" w:color="auto"/>
            </w:pBdr>
            <w:shd w:val="clear" w:color="auto" w:fill="FFFF00"/>
            <w:tabs>
              <w:tab w:val="left" w:pos="720"/>
            </w:tabs>
            <w:wordWrap w:val="0"/>
            <w:autoSpaceDE w:val="0"/>
            <w:bidi/>
            <w:contextualSpacing/>
            <w:mirrorIndents/>
            <w:jc w:val="left"/>
            <w:textDirection w:val="btLr"/>
            <w:textAlignment w:val="center"/>
          </w:pPr>
        </w:pPrChange>
      </w:pPr>
      <w:r><w:t>Snapshot-bearing paragraph.</w:t></w:r>
    </w:p>"#
}

fn wrap_body(body_content: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"
            mc:Ignorable="w14">
  <w:body>
{body_content}
    <w:sectPr/>
  </w:body>
</w:document>"#
    )
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut z = zip::ZipArchive::new(Cursor::new(docx.to_vec())).expect("zip");
    let mut f = z.by_name("word/document.xml").expect("document.xml");
    let mut xml = String::new();
    f.read_to_string(&mut xml).expect("read");
    xml
}

fn first_paragraph(doc: &Document) -> ParagraphNode {
    let canon = doc.snapshot().canonical.clone();
    canon
        .blocks
        .iter()
        .find_map(|b| match &b.block {
            BlockNode::Paragraph(p) => Some((**p).clone()),
            _ => None,
        })
        .expect("document has a paragraph")
}

// ════════════════════════════════════════════════════════════════════════════
// (a) Import parses the snapshot into the MODELED previous_* fields — they
//     must not be hardcoded to "absent" — and the preserved remainder holds
//     ONLY genuinely unmodeled children.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn import_models_the_snapshot_fields_instead_of_hardcoding_them_absent() {
    let docx = build_docx(&wrap_body(snapshot_bearing_body()));
    let doc = Document::parse(&docx).expect("parse snapshot-bearing doc");
    let p = first_paragraph(&doc);
    let fc = p
        .formatting_change
        .as_ref()
        .expect("paragraph carries the imported pPrChange");

    assert_eq!(
        fc.previous_style_id.as_deref(),
        Some("Quote"),
        "previous pStyle must be modeled"
    );
    assert_eq!(fc.previous_keep_next, Some(true), "previous keepNext");
    assert_eq!(
        fc.previous_keep_lines,
        Some(false),
        "previous keepLines w:val=0"
    );
    assert!(fc.previous_page_break_before, "previous pageBreakBefore");
    assert_eq!(
        fc.previous_widow_control,
        Some(false),
        "previous widowControl w:val=0"
    );
    assert_eq!(
        fc.previous_contextual_spacing,
        Some(true),
        "previous contextualSpacing"
    );
    let shading = fc
        .previous_shading
        .as_ref()
        .expect("previous shd must be modeled");
    assert_eq!(shading.fill.as_deref(), Some("FFFF00"));
    let borders = fc
        .previous_borders
        .as_ref()
        .expect("previous pBdr must be modeled");
    assert!(borders.top.is_some(), "previous pBdr top edge");
    assert_eq!(fc.previous_tab_stops.len(), 1, "previous tabs");
    assert_eq!(fc.previous_tab_stops[0].position, 720);
    assert_eq!(
        fc.previous_text_direction,
        Some(TextDirection::BtLr),
        "previous textDirection"
    );
    assert_eq!(
        fc.previous_text_alignment,
        Some(TextAlignment::Center),
        "previous textAlignment"
    );
    assert_eq!(
        fc.previous_mirror_indents,
        Some(true),
        "previous mirrorIndents"
    );
    assert_eq!(fc.previous_bidi, Some(true), "previous bidi");
    assert_eq!(
        fc.previous_auto_space_de,
        Some(false),
        "previous autoSpaceDE w:val=0"
    );
    assert_eq!(
        fc.previous_word_wrap,
        Some(false),
        "previous wordWrap w:val=0"
    );
    let frame = fc
        .previous_frame_pr
        .as_ref()
        .expect("previous framePr must be modeled");
    assert_eq!(frame.width, Some(2000));
    assert_eq!(frame.height, Some(400));

    // The preserved remainder now holds ONLY the genuinely unmodeled
    // children. A modeled child left in the bag would double-write on
    // serialization (once from the typed field, once verbatim).
    let bag_names: Vec<&str> = fc
        .previous_preserved_ppr
        .iter()
        .map(|prop| prop.name.as_str())
        .collect();
    assert!(
        bag_names.contains(&"w:suppressLineNumbers"),
        "unmodeled w:suppressLineNumbers stays preserved verbatim: {bag_names:?}"
    );
    assert!(
        bag_names.contains(&"w:numPr"),
        "w:numPr (no previous_numbering synthesis at import) stays preserved verbatim: {bag_names:?}"
    );
    for modeled in [
        "w:pStyle",
        "w:keepNext",
        "w:keepLines",
        "w:pageBreakBefore",
        "w:framePr",
        "w:widowControl",
        "w:pBdr",
        "w:shd",
        "w:tabs",
        "w:wordWrap",
        "w:autoSpaceDE",
        "w:bidi",
        "w:contextualSpacing",
        "w:mirrorIndents",
        "w:textDirection",
        "w:textAlignment",
    ] {
        assert!(
            !bag_names.contains(&modeled),
            "{modeled} has a previous_* domain field and must NOT also sit in \
             the preserved bag (double-write hazard): {bag_names:?}"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// (b) Reject restores the snapshot as MODEL state, not just as raw XML in the
//     preserved bag. This is the reversibility contract of §17.13.5.29: after
//     reject, model consumers (read_format, style resolution, later edits)
//     must see the previous formatting.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn reject_restores_the_snapshot_as_model_state() {
    let docx = build_docx(&wrap_body(snapshot_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");
    let resolved = doc
        .project(Resolution::RejectAll)
        .expect("reject-all must succeed");
    let p = first_paragraph(&resolved);

    assert!(
        p.formatting_change.is_none(),
        "reject removes the change record"
    );
    assert_eq!(
        p.style_id.as_deref(),
        Some("Quote"),
        "reject must restore the previous pStyle as model state"
    );
    assert_eq!(p.keep_next, Some(true), "restored keepNext");
    assert_eq!(p.keep_lines, Some(false), "restored keepLines w:val=0");
    assert!(p.page_break_before, "restored pageBreakBefore");
    assert_eq!(p.widow_control, Some(false), "restored widowControl");
    assert_eq!(
        p.contextual_spacing,
        Some(true),
        "restored contextualSpacing"
    );
    assert_eq!(
        p.shading.as_ref().and_then(|s| s.fill.as_deref()),
        Some("FFFF00"),
        "restored shd fill"
    );
    assert!(
        p.borders.as_ref().is_some_and(|b| b.top.is_some()),
        "restored pBdr top edge"
    );
    assert_eq!(p.tab_stops.len(), 1, "restored tabs");
    assert_eq!(p.tab_stops[0].position, 720);
    assert_eq!(p.text_direction, Some(TextDirection::BtLr));
    assert_eq!(p.text_alignment, Some(TextAlignment::Center));
    assert_eq!(p.mirror_indents, Some(true), "restored mirrorIndents");
    assert_eq!(p.bidi, Some(true), "restored bidi");
    assert_eq!(p.auto_space_de, Some(false), "restored autoSpaceDE");
    assert_eq!(p.word_wrap, Some(false), "restored wordWrap");
    assert_eq!(
        p.frame_pr.as_ref().and_then(|f| f.width),
        Some(2000),
        "restored framePr width"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (c) Serialized reject output carries each restored property EXACTLY ONCE on
//     the paragraph's own pPr — from the typed field, with no verbatim
//     double-write from the preserved bag.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn rejected_output_writes_each_restored_property_exactly_once() {
    let docx = build_docx(&wrap_body(snapshot_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");
    let resolved = doc
        .project(Resolution::RejectAll)
        .expect("reject-all must succeed");
    let bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&bytes);

    assert!(
        !xml.contains("w:pPrChange"),
        "reject-all must remove the pPrChange record: {xml}"
    );
    for (needle, label) in [
        ("<w:pStyle", "pStyle"),
        ("<w:keepNext", "keepNext"),
        ("<w:keepLines", "keepLines"),
        ("<w:pageBreakBefore", "pageBreakBefore"),
        ("<w:framePr", "framePr"),
        ("<w:widowControl", "widowControl"),
        ("<w:pBdr", "pBdr"),
        ("<w:shd", "shd"),
        ("<w:tabs", "tabs"),
        ("<w:wordWrap", "wordWrap"),
        ("<w:autoSpaceDE", "autoSpaceDE"),
        ("<w:bidi", "bidi"),
        ("<w:contextualSpacing", "contextualSpacing"),
        ("<w:mirrorIndents", "mirrorIndents"),
        ("<w:textDirection", "textDirection"),
        ("<w:textAlignment", "textAlignment"),
        ("<w:suppressLineNumbers", "suppressLineNumbers"),
        ("<w:numPr", "numPr"),
    ] {
        let count = xml.matches(needle).count();
        assert_eq!(
            count, 1,
            "restored {label} must appear exactly once in the rejected \
             output (0 = dropped, 2 = modeled+preserved double-write): {xml}"
        );
    }
}
