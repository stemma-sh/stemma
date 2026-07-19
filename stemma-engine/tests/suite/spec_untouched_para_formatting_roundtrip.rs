//! Direct paragraph/run formatting must round-trip VERBATIM on a whole-document
//! rebuild, even for paragraphs the edit never touched.
//!
//! A `diff_and_redline` export rebuilds every block from the model. The model
//! used to store the RESOLVED EFFECTIVE `w:ind`/`w:spacing` (numbering-level +
//! style-chain cascade baked in, plus import-time rendering transforms like tab
//! absorption) in the same field the serializer re-emitted as DIRECT pPr. So an
//! untouched paragraph's authored `w:ind`/`w:spacing`/run `w:u` was not faithful
//! after any unrelated edit:
//!
//! - a numbered paragraph carrying only `<w:ind w:right="1228"/>` re-emitted with
//!   an INJECTED `w:left` materialized from the numbering-level indent (rendered
//!   indent halved);
//! - a paragraph carrying `<w:ind w:left="1886" w:hanging="1886"/>` whose body had
//!   a tab re-emitted as `<w:ind w:left="0"/>` (tab absorption dropped the hanging);
//! - a run carrying `<w:u w:val="none"/>` (an explicit override cancelling a style
//!   underline) re-emitted with NO `w:u`, so the style underline resurfaced;
//! - a paragraph carrying `<w:spacing w:line="300" w:lineRule="atLeast"/>` re-emitted
//!   with an INJECTED `w:after` baked from the style's space-after.
//!
//! The fix distinguishes the AUTHORED-direct value (what the serializer emits,
//! verbatim) from the RESOLVED EFFECTIVE value (the frontend/layout projection) —
//! the paragraph analogue of the `tab_stops` vs `effective_tab_stops_rel` split,
//! and the underline analogue of `bold_off`/`italic_off`. These tests pin the
//! invariant from the domain rule, hermetically (in-memory `.docx`, no corpus).

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::edit::*;
use stemma::{ExportOptions, RevisionInfo};
use zip::ZipWriter;
use zip::write::FileOptions;

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
  <Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>
</Types>"#;

const RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const WORD_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>
</Relationships>"#;

/// Build a `.docx` with the given document/styles/numbering parts. `styles` and
/// `numbering` are always written (empty-ish defaults) so the fixed content-type
/// / rels wiring stays valid.
fn build_docx(document_xml: &str, styles_xml: &str, numbering_xml: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();
    for (path, body) in [
        ("[Content_Types].xml", CONTENT_TYPES_XML),
        ("_rels/.rels", RELS_XML),
        ("word/_rels/document.xml.rels", WORD_RELS_XML),
        ("word/document.xml", document_xml),
        ("word/styles.xml", styles_xml),
        ("word/numbering.xml", numbering_xml),
    ] {
        zip.start_file(path, options).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
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

const EMPTY_STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"></w:styles>"#;

const EMPTY_NUMBERING: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"></w:numbering>"#;

fn document_xml_of(docx: &[u8]) -> String {
    let mut z = zip::ZipArchive::new(Cursor::new(docx.to_vec())).expect("zip");
    let mut f = z.by_name("word/document.xml").expect("document.xml");
    let mut xml = String::new();
    f.read_to_string(&mut xml).expect("read");
    xml
}

/// The `<w:pPr>…</w:pPr>` substring for the paragraph whose text contains `needle`.
fn ppr_of_paragraph<'a>(xml: &'a str, needle: &str) -> &'a str {
    let at = xml
        .find(needle)
        .unwrap_or_else(|| panic!("paragraph {needle:?} present: {xml}"));
    let ppr_start = xml[..at]
        .rfind("<w:pPr")
        .unwrap_or_else(|| panic!("{needle:?} has a pPr: {xml}"));
    let ppr_end = xml[ppr_start..]
        .find("</w:pPr>")
        .map(|e| ppr_start + e + "</w:pPr>".len())
        .unwrap_or_else(|| panic!("{needle:?} pPr closes: {xml}"));
    &xml[ppr_start..ppr_end]
}

/// The `<w:r>…</w:r>` substring for the run whose text is exactly `text`.
fn run_of_text<'a>(xml: &'a str, text: &str) -> &'a str {
    let marker = format!("<w:t>{text}</w:t>");
    let at = xml
        .find(&marker)
        .or_else(|| xml.find(&format!("<w:t xml:space=\"preserve\">{text}</w:t>")))
        .unwrap_or_else(|| panic!("run text {text:?} present: {xml}"));
    let run_start = xml[..at]
        .rfind("<w:r>")
        .or_else(|| {
            xml[..at]
                .rfind("<w:r ")
                .map(|_| xml[..at].rfind("<w:r").unwrap())
        })
        .unwrap_or_else(|| panic!("{text:?} in a run: {xml}"));
    let run_end = xml[run_start..]
        .find("</w:r>")
        .map(|e| run_start + e)
        .unwrap_or_else(|| panic!("{text:?} run closes: {xml}"));
    &xml[run_start..run_end]
}

/// Apply a bold-formatting edit to the run whose text is `target_text`, forcing a
/// full whole-document re-serialize. Returns the serialized bytes.
fn edit_a_different_paragraph(doc: &Document, target_text: &str) -> Vec<u8> {
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains(target_text))
        .unwrap_or_else(|| panic!("target paragraph {target_text:?} present"))
        .id
        .clone();
    let txn = EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: target,
            expect: target_text.to_string(),
            semantic_hash: None,
            marks: InlineMarkSet {
                bold: true,
                ..Default::default()
            },
            style: RunStyleEdit::default(),
            rationale: None,
        }],
        summary: Some("unrelated edit to a different paragraph".to_string()),
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Spec".to_string()),
            date: Some("2026-07-06T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    let edited = doc.apply(&txn).expect("apply unrelated edit");
    edited
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

// ────────────────────────────────────────────────────────────────────────────
// A. w:ind — inject direction: a numbered paragraph with only direct w:right must
//    NOT gain a w:left materialized from the numbering-level indent.
// ────────────────────────────────────────────────────────────────────────────

const NUMBERING_LEFT_360: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
      <w:pPr><w:ind w:left="360" w:hanging="360"/></w:pPr>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;

#[test]
fn numbered_paragraph_with_only_right_does_not_materialize_inherited_left() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
        <w:ind w:right="1228"/>
      </w:pPr>
      <w:r><w:t>NumberedItem</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, NUMBERING_LEFT_360);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);

    let ppr = ppr_of_paragraph(&xml, "NumberedItem");
    assert!(
        ppr.contains(r#"w:right="1228""#),
        "the authored w:right must survive: {ppr}"
    );
    assert!(
        !ppr.contains("w:left="),
        "no w:left may be materialized from the numbering-level indent onto the \
         direct pPr — the numbering owns the left indent (would halve the rendered \
         indent): {ppr}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// B. w:ind — drop direction: an authored hanging indent must survive a rebuild
//    even when the paragraph's body has a tab (which absorbs firstLine into the
//    EFFECTIVE indent for rendering — a projection that must not reach the wire).
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn hanging_indent_survives_rebuild_despite_body_tab_absorption() {
    // left=1886 hanging=1886, and a tab in the body → the effective indent is
    // tab-absorbed to left=0/firstLine=None for the frontend. The authored w:ind
    // must still re-emit verbatim.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:ind w:left="1886" w:hanging="1886"/>
      </w:pPr>
      <w:r><w:t xml:space="preserve">Label</w:t></w:r>
      <w:r><w:tab/></w:r>
      <w:r><w:t xml:space="preserve">Clause body</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");

    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let ppr = ppr_of_paragraph(&xml, "Clause body");
    assert!(
        ppr.contains(r#"w:left="1886""#) && ppr.contains(r#"w:hanging="1886""#),
        "authored left+hanging must round-trip verbatim (tab absorption is a render \
         projection, not the wire value); must NOT collapse to w:left=0: {ppr}"
    );
    assert!(
        !ppr.contains(r#"w:left="0""#),
        "the tab-absorbed effective left=0 must not reach the serialized w:ind: {ppr}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// C. w:ind — an explicit twip left="0" on a numbered paragraph is a real override
//    (cancels the numbering-level left) and must re-emit, not vanish.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn explicit_left_zero_overrides_numbering_and_is_preserved() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
        <w:ind w:left="0"/>
      </w:pPr>
      <w:r><w:t>ZeroLeft</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, NUMBERING_LEFT_360);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let ppr = ppr_of_paragraph(&xml, "ZeroLeft");
    assert!(
        ppr.contains(r#"w:left="0""#),
        "an explicit direct left=0 (cancelling the numbering's left=360) must be \
         preserved — dropping it lets Word re-apply the inherited left: {ppr}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// D. run w:u w:val="none" — an explicit underline OFF override cancelling a style
//    underline must re-emit; the style underline must not resurface.
// ────────────────────────────────────────────────────────────────────────────

const STYLES_UNDERLINE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="BodyU">
    <w:name w:val="Body Underlined"/>
    <w:rPr><w:u w:val="single"/></w:rPr>
  </w:style>
</w:styles>"#;

#[test]
fn run_underline_none_override_survives_roundtrip() {
    let body = r#"
    <w:p>
      <w:pPr><w:pStyle w:val="BodyU"/></w:pPr>
      <w:r><w:rPr><w:u w:val="none"/></w:rPr><w:t>NotUnderlined</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_UNDERLINE, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");

    // A plain parse→serialize already exercises the run rPr emission.
    let out = doc.serialize(&ExportOptions::default()).expect("serialize");
    let xml = document_xml_of(&out);
    let run = run_of_text(&xml, "NotUnderlined");
    assert!(
        run.contains(r#"<w:u w:val="none""#),
        "the run's explicit underline OFF override must re-emit as <w:u w:val=\"none\"/> \
         so the style's single underline does not resurface: {run}"
    );
}

#[test]
fn run_underline_none_survives_unrelated_edit() {
    let body = r#"
    <w:p>
      <w:pPr><w:pStyle w:val="BodyU"/></w:pPr>
      <w:r><w:rPr><w:u w:val="none"/></w:rPr><w:t>NotUnderlined</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_UNDERLINE, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let run = run_of_text(&xml, "NotUnderlined");
    assert!(
        run.contains(r#"<w:u w:val="none""#),
        "the underline OFF override must survive a whole-document rebuild: {run}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// E. w:spacing — a partial direct w:spacing must NOT gain a w:after materialized
//    from the style's space-after; authored values re-emit verbatim.
// ────────────────────────────────────────────────────────────────────────────

const STYLES_SPACE_AFTER: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="BodyS">
    <w:name w:val="Body Spaced"/>
    <w:pPr><w:spacing w:after="160"/></w:pPr>
  </w:style>
</w:styles>"#;

#[test]
fn partial_spacing_does_not_materialize_inherited_after() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:pStyle w:val="BodyS"/>
        <w:spacing w:line="300" w:lineRule="atLeast"/>
      </w:pPr>
      <w:r><w:t>SpacedClause</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_SPACE_AFTER, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let ppr = ppr_of_paragraph(&xml, "SpacedClause");
    assert!(
        ppr.contains(r#"w:line="300""#) && ppr.contains(r#"w:lineRule="atLeast""#),
        "authored line spacing must round-trip verbatim: {ppr}"
    );
    assert!(
        !ppr.contains("w:after="),
        "the style's w:after=160 must NOT be baked into the direct w:spacing — the \
         paragraph authored only line/lineRule (rendered space-after would jump 0→8pt): {ppr}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// F. Reject fixpoint: rejecting the redline restores the untouched paragraph's
//    authored w:ind verbatim and leaves every paragraph's text at the original.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn reject_restores_untouched_ind_and_all_texts() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
        <w:ind w:right="1228"/>
      </w:pPr>
      <w:r><w:t>NumberedItem</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, NUMBERING_LEFT_360);
    let doc = Document::parse(&docx).expect("parse");

    // Author a TRACKED edit on the OTHER paragraph, serialize the redline, reimport.
    let view = doc.read();
    let editable = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Editable"))
        .expect("editable paragraph")
        .id
        .clone();
    let txn = EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: editable,
            expect: "Editable".to_string(),
            semantic_hash: None,
            marks: InlineMarkSet {
                bold: true,
                ..Default::default()
            },
            style: RunStyleEdit::default(),
            rationale: None,
        }],
        summary: Some("tracked edit".to_string()),
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 7,
            identity: 0,
            author: Some("Spec".to_string()),
            date: Some("2026-07-06T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    let redlined = doc.apply(&txn).expect("apply tracked edit");
    let redline_bytes = redlined
        .serialize(&ExportOptions::default())
        .expect("serialize redline");

    // Reimport the redline and reject every change.
    let reimported = Document::parse(&redline_bytes).expect("reparse redline");
    let rejected = reimported.read_rejected().expect("reject all");
    let out = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize rejected");
    let xml = document_xml_of(&out);

    // The untouched numbered paragraph is verbatim: right kept, no left injected.
    let ppr = ppr_of_paragraph(&xml, "NumberedItem");
    assert!(
        ppr.contains(r#"w:right="1228""#) && !ppr.contains("w:left="),
        "reject must leave the untouched paragraph's authored w:ind verbatim: {ppr}"
    );
    // Every original text is present (reject removed the tracked change cleanly).
    assert!(xml.contains("NumberedItem") && xml.contains("Editable"));
}

// ────────────────────────────────────────────────────────────────────────────
// G. Two-cycle idempotence: parse→serialize→parse→serialize leaves the authored
//    pPr byte-stable for the affected attributes.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn two_cycle_idempotence_of_authored_ppr() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
        <w:ind w:right="1228"/>
      </w:pPr>
      <w:r><w:t>NumberedItem</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_SPACE_AFTER, NUMBERING_LEFT_360);

    let cycle = |bytes: &[u8]| -> Vec<u8> {
        Document::parse(bytes)
            .expect("parse")
            .serialize(&ExportOptions::default())
            .expect("serialize")
    };
    let once = cycle(&docx);
    let twice = cycle(&once);
    let ppr1 = ppr_of_paragraph(&document_xml_of(&once), "NumberedItem").to_string();
    let ppr2 = ppr_of_paragraph(&document_xml_of(&twice), "NumberedItem").to_string();
    assert_eq!(
        ppr1, ppr2,
        "the authored pPr must be a fixpoint across a second round-trip"
    );
    assert!(
        !ppr1.contains("w:left=") && ppr1.contains(r#"w:right="1228""#),
        "and it must be the faithful authored form (no injected left): {ppr1}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// H. Paragraph-MARK rPr (w:pPr/w:rPr, §17.3.1.29 CT_ParaRPr). The pilcrow's own
//    formatting is stored presence-only (`paragraph_mark_marks: Vec<Mark>`) +
//    value props, so an explicit OFF override the mark authored — `<w:u
//    w:val="none"/>`, `<w:b w:val="0"/>`, `<w:i w:val="0"/>` — could not be
//    represented and dropped on every rebuild (the run-level twin of section D,
//    fixed there but not for the paragraph mark). These pin the OFF forms.
// ────────────────────────────────────────────────────────────────────────────

/// The `<w:rPr>…</w:rPr>` that lives DIRECTLY inside the pPr for the paragraph
/// whose body contains `needle` — i.e. the paragraph-mark (pilcrow) rPr, not a
/// run rPr. Isolated so an assertion can't accidentally read a body run's rPr.
fn para_mark_rpr_of<'a>(xml: &'a str, needle: &str) -> &'a str {
    let ppr = ppr_of_paragraph(xml, needle);
    let rpr_start = ppr
        .find("<w:rPr")
        .unwrap_or_else(|| panic!("{needle:?} pPr has a paragraph-mark rPr: {ppr}"));
    let rpr_end = ppr[rpr_start..]
        .find("</w:rPr>")
        .map(|e| rpr_start + e + "</w:rPr>".len())
        .unwrap_or_else(|| panic!("{needle:?} paragraph-mark rPr closes: {ppr}"));
    &ppr[rpr_start..rpr_end]
}

#[test]
fn para_mark_underline_none_override_survives_roundtrip() {
    // The pilcrow authors `<w:u w:val="none"/>` (cancelling the style's single
    // underline). A plain parse→serialize must re-emit it inside pPr/rPr.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:pStyle w:val="BodyU"/>
        <w:rPr><w:u w:val="none"/></w:rPr>
      </w:pPr>
      <w:r><w:t>MarkNoUnderline</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_UNDERLINE, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = doc.serialize(&ExportOptions::default()).expect("serialize");
    let xml = document_xml_of(&out);
    let rpr = para_mark_rpr_of(&xml, "MarkNoUnderline");
    assert!(
        rpr.contains(r#"<w:u w:val="none""#),
        "the paragraph mark's explicit underline OFF must re-emit as \
         <w:u w:val=\"none\"/> so the style underline does not resurface on the \
         pilcrow: {rpr}"
    );
}

#[test]
fn para_mark_underline_none_survives_unrelated_edit() {
    // Same override, but forced through a whole-document rebuild by editing a
    // DIFFERENT paragraph — the path that was silently dropping it.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:pStyle w:val="BodyU"/>
        <w:rPr><w:u w:val="none"/></w:rPr>
      </w:pPr>
      <w:r><w:t>MarkNoUnderline</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_UNDERLINE, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let rpr = para_mark_rpr_of(&xml, "MarkNoUnderline");
    assert!(
        rpr.contains(r#"<w:u w:val="none""#),
        "the paragraph mark's underline OFF must survive a whole-document rebuild: {rpr}"
    );
}

#[test]
fn para_mark_bold_and_italic_off_survive_rebuild() {
    // `<w:b w:val="0"/>` / `<w:i w:val="0"/>` on the pilcrow — a presence-only
    // Vec<Mark> cannot carry them, so without explicit OFF representation they
    // dropped entirely (not even captured at import).
    let body = r#"
    <w:p>
      <w:pPr>
        <w:rPr><w:b w:val="0"/><w:i w:val="0"/></w:rPr>
      </w:pPr>
      <w:r><w:t>MarkOffToggles</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let rpr = para_mark_rpr_of(&xml, "MarkOffToggles");
    assert!(
        rpr.contains(r#"<w:b w:val="0""#) && rpr.contains(r#"<w:i w:val="0""#),
        "the paragraph mark's authored bold/italic OFF toggles must round-trip \
         verbatim: {rpr}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// I. w:framePr (§17.3.1.11 CT_FramePr). The frame model used to carry only
//    w/h/hRule/hSpace/wrap/vAnchor/hAnchor/x — note x was modeled but y was
//    not — so a whole-document rebuild silently dropped every other CT_FramePr
//    attribute: the relative-alignment pair (xAlign/yAlign), the absolute y and
//    vertical spacing (y/vSpace), and the drop-cap / anchor-lock remainder
//    (dropCap/lines/anchorLock). The classic framed page-number in a header
//    carries `w:xAlign="center" w:y="1"`; dropping those un-centers the page
//    number on any edit that reserializes the story. These pin the full set:
//    the typed additions survive with their values, and the unmodeled
//    remainder round-trips verbatim.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn framepr_full_attribute_set_survives_unrelated_edit() {
    // A framed paragraph carrying the full CT_FramePr surface: modeled geometry
    // (w/h), the previously-modeled x, the newly-typed xAlign/y/yAlign/vSpace,
    // and the verbatim remainder (dropCap/lines/anchorLock). An edit to a
    // DIFFERENT paragraph forces a whole-document rebuild.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:framePr w:w="2000" w:h="400" w:hRule="exact" w:hSpace="180" w:vSpace="120"
                   w:wrap="around" w:vAnchor="text" w:hAnchor="margin"
                   w:x="720" w:xAlign="center" w:y="1" w:yAlign="top"
                   w:dropCap="drop" w:lines="3" w:anchorLock="1"/>
      </w:pPr>
      <w:r><w:t>Framed</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let ppr = ppr_of_paragraph(&xml, "Framed");

    // The typed additions must survive with their authored values. y is the
    // counterpart to the already-modeled x; both must appear.
    for (attr, value) in [
        ("w:xAlign", "center"),
        ("w:y", "1"),
        ("w:yAlign", "top"),
        ("w:vSpace", "120"),
        ("w:x", "720"),
    ] {
        assert!(
            ppr.contains(&format!(r#"{attr}="{value}""#)),
            "§17.3.1.11: framePr {attr}=\"{value}\" must survive a whole-document \
             rebuild (was silently dropped before it was modeled): {ppr}"
        );
    }
    // The unmodeled remainder must round-trip verbatim via extra_attrs.
    for (attr, value) in [
        ("w:dropCap", "drop"),
        ("w:lines", "3"),
        ("w:anchorLock", "1"),
    ] {
        assert!(
            ppr.contains(&format!(r#"{attr}="{value}""#)),
            "§17.3.1.11: unmodeled framePr {attr}=\"{value}\" must round-trip \
             verbatim through the extra_attrs remainder: {ppr}"
        );
    }
}

#[test]
fn framepr_in_pprchange_previous_ppr_survives_rebuild() {
    // The framePr the paragraph carried BEFORE a tracked formatting change lives
    // inside w:pPrChange/w:pPr (§17.13.5.29). It reaches the wire through the
    // second serializer emit site (previous_frame_pr), a different path from the
    // live pPr, and the tracked-change accept/reject must be able to restore the
    // full frame. A whole-document rebuild (unrelated edit) must re-emit the
    // previous framePr with every CT_FramePr attribute intact — pre-fix it kept
    // only w/h/hRule/hSpace/wrap/vAnchor/hAnchor/x, un-centering the restored
    // page number on reject.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:framePr w:wrap="around" w:vAnchor="text" w:hAnchor="margin"/>
        <w:pPrChange w:id="4" w:author="Reviewer" w:date="2026-07-09T00:00:00Z">
          <w:pPr>
            <w:framePr w:w="2000" w:h="400" w:hRule="exact" w:hSpace="180" w:vSpace="120"
                       w:wrap="around" w:vAnchor="text" w:hAnchor="page"
                       w:x="720" w:xAlign="center" w:y="1" w:yAlign="top"
                       w:dropCap="drop" w:lines="2" w:anchorLock="1"/>
          </w:pPr>
        </w:pPrChange>
      </w:pPr>
      <w:r><w:t>FramedChange</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let ppr = ppr_of_paragraph(&xml, "FramedChange");

    // The pPrChange's previous framePr must carry every attribute. The live
    // framePr authored only wrap/vAnchor/hAnchor, so any of these appearing in
    // the pPr can only have come from the pPrChange's previous pPr.
    for needle in [
        r#"w:xAlign="center""#,
        r#"w:y="1""#,
        r#"w:yAlign="top""#,
        r#"w:vSpace="120""#,
        r#"w:x="720""#,
        r#"w:dropCap="drop""#,
        r#"w:lines="2""#,
        r#"w:anchorLock="1""#,
    ] {
        assert!(
            ppr.contains(needle),
            "§17.3.1.11: the pPrChange previous framePr must re-emit {needle} through \
             the previous_frame_pr path so accept/reject restores the full frame: {ppr}"
        );
    }
}

#[test]
fn para_mark_and_body_run_underline_none_both_survive() {
    // The wild-witness shape: `<w:u w:val="none"/>` appears BOTH on a body run
    // (fixed by the run-rPr round-trip) AND inside pPr/rPr (the pilcrow). Both
    // occurrences must survive a whole-document rebuild.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:pStyle w:val="BodyU"/>
        <w:rPr><w:u w:val="none"/></w:rPr>
      </w:pPr>
      <w:r><w:rPr><w:u w:val="none"/></w:rPr><w:t>BothNoUnderline</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_UNDERLINE, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);
    let count = xml.matches(r#"<w:u w:val="none""#).count();
    assert!(
        count >= 2,
        "both the body-run AND the paragraph-mark <w:u w:val=\"none\"/> must survive \
         the rebuild (found {count}): {xml}"
    );
}
