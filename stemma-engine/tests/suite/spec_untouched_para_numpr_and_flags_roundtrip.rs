//! numPr and CT_OnOff pPr FLAGS must round-trip VERBATIM on a whole-document
//! rebuild, even for paragraphs the edit never touched.
//!
//! A `diff_and_redline` export rebuilds every block from the model. Two classes
//! of silent formatting drift on UNTOUCHED paragraphs are pinned here:
//!
//! Class E — numPr. The model's `numbering` field stores the RESOLVED EFFECTIVE
//! numbering (direct §17.7.4.14, then style, then the abstractNum's `<w:pStyle>`
//! reverse binding §17.9.23). The serializer used to emit it as a DIRECT
//! `w:numPr` unconditionally, so a paragraph whose numbering was INHERITED from
//! its style (no direct numPr of its own) gained a materialized `w:numPr` on
//! rebuild — changing its numbering-inherited indent with no `pPrChange`. The
//! fix gates emission on `has_direct_numbering` (the numbering analogue of
//! `has_direct_indent`).
//!
//! Class F — CT_OnOff flags (bidi, pageBreakBefore, wordWrap, overflowPunct,
//! suppressAutoHyphens, snapToGrid, adjustRightInd, mirrorIndents, …). The
//! serializer was one-armed: it emitted only one polarity per flag, and `bidi`/
//! `mirror_indents` were lossy plain `bool`s that could not carry an explicit
//! OFF. An explicit `w:val="0"` (or an explicit ON like `<w:wordWrap/>`) is an
//! AUTHORED override, not the same as absent (§17.17.4 ST_OnOff) — dropping it
//! let the paragraph re-inherit its style's value. The fix emits BOTH polarities
//! for every flag and models `bidi`/`mirror_indents` as three-state.
//!
//! These tests pin each direction from the domain rule, hermetically (in-memory
//! `.docx`, no corpus).

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

/// Apply a bold-formatting edit to the run whose text is `target_text` (a
/// DIFFERENT paragraph than the ones under test), forcing a whole-document
/// re-serialize. Returns the serialized bytes.
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

// A numbering definition bound to the `Heading` paragraph style via the
// abstractNum's `<w:pStyle>` reverse link (§17.9.23): a paragraph carrying
// `<w:pStyle w:val="Heading"/>` and NO direct numPr resolves to numId=1/ilvl=0.
const NUMBERING_PSTYLE_BOUND: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
      <w:pStyle w:val="Heading"/>
      <w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;

const STYLES_HEADING: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading">
    <w:name w:val="Heading"/>
  </w:style>
</w:styles>"#;

// ────────────────────────────────────────────────────────────────────────────
// Class E — numPr: numbering INHERITED via the pStyle reverse binding must NOT
// be materialized as a direct w:numPr on an untouched paragraph.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn pstyle_bound_numbering_is_not_materialized_as_direct_numpr() {
    let body = r#"
    <w:p>
      <w:pPr><w:pStyle w:val="Heading"/></w:pPr>
      <w:r><w:t>InheritedNumbered</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_HEADING, NUMBERING_PSTYLE_BOUND);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);

    let ppr = ppr_of_paragraph(&xml, "InheritedNumbered");
    assert!(
        !ppr.contains("<w:numPr"),
        "numbering bound to the paragraph's pStyle (via the abstractNum's \
         <w:pStyle>) is INHERITED, not direct — it must not be materialized onto \
         the paragraph's own pPr on rebuild (Word re-derives it from the style, \
         and a hard-coded direct numPr shifts the numbering-inherited indent): {ppr}"
    );
    // The paragraph keeps its style binding, so Word still renders the number.
    assert!(
        ppr.contains(r#"<w:pStyle w:val="Heading""#),
        "the pStyle that carries the numbering must survive: {ppr}"
    );
}

#[test]
fn directly_authored_numpr_still_round_trips() {
    // The direct-numPr case must be UNAFFECTED by the has_direct_numbering gate.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
      </w:pPr>
      <w:r><w:t>DirectNumbered</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_HEADING, NUMBERING_PSTYLE_BOUND);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);

    let ppr = ppr_of_paragraph(&xml, "DirectNumbered");
    assert!(
        ppr.contains("<w:numPr") && ppr.contains(r#"<w:numId w:val="1""#),
        "a paragraph that authored its OWN w:numPr must keep emitting it: {ppr}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Class F — CT_OnOff flags: both polarities of every flag must round-trip on an
// untouched paragraph. Explicit OFF cancels an inherited flag; explicit ON is
// equally authored.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn explicit_off_flags_survive_rebuild() {
    // bidi=0, pageBreakBefore=0, snapToGrid=0, suppressAutoHyphens=0,
    // mirrorIndents=0 — each an explicit OFF override. All previously dropped
    // (bidi/pageBreakBefore/mirrorIndents could not carry OFF at all;
    // suppressAutoHyphens emitted only ON; snapToGrid only OFF — so THAT one
    // survived, and is the control).
    let body = r#"
    <w:p>
      <w:pPr>
        <w:pageBreakBefore w:val="0"/>
        <w:suppressAutoHyphens w:val="0"/>
        <w:snapToGrid w:val="0"/>
        <w:bidi w:val="0"/>
        <w:mirrorIndents w:val="0"/>
      </w:pPr>
      <w:r><w:t>ExplicitOff</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);

    let ppr = ppr_of_paragraph(&xml, "ExplicitOff");
    for (name, frag) in [
        ("pageBreakBefore", r#"<w:pageBreakBefore w:val="0""#),
        ("suppressAutoHyphens", r#"<w:suppressAutoHyphens w:val="0""#),
        ("snapToGrid", r#"<w:snapToGrid w:val="0""#),
        ("bidi", r#"<w:bidi w:val="0""#),
        ("mirrorIndents", r#"<w:mirrorIndents w:val="0""#),
    ] {
        assert!(
            ppr.contains(frag),
            "explicit OFF of {name} must round-trip verbatim (an authored override \
             is not the same as absent): {ppr}"
        );
    }
}

#[test]
fn explicit_on_flags_survive_rebuild() {
    // wordWrap ON and overflowPunct ON (the East-Asian default-on flags an
    // author writes explicitly). The serializer used to emit these only in their
    // OFF form, so an explicit ON was silently dropped.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:wordWrap/>
        <w:overflowPunct/>
      </w:pPr>
      <w:r><w:t>ExplicitOn</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);

    let ppr = ppr_of_paragraph(&xml, "ExplicitOn");
    assert!(
        ppr.contains("<w:wordWrap"),
        "explicit ON of wordWrap must round-trip: {ppr}"
    );
    assert!(
        ppr.contains("<w:overflowPunct"),
        "explicit ON of overflowPunct must round-trip: {ppr}"
    );
    // And they must NOT flip to an OFF form.
    assert!(
        !ppr.contains(r#"<w:wordWrap w:val="0""#) && !ppr.contains(r#"<w:overflowPunct w:val="0""#),
        "an authored ON must not be emitted as an OFF: {ppr}"
    );
}

#[test]
fn absent_flags_are_not_materialized() {
    // A bare paragraph authors no flags; none may be injected on rebuild.
    let body = r#"
    <w:p>
      <w:r><w:t>Bare</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), EMPTY_STYLES, EMPTY_NUMBERING);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_a_different_paragraph(&doc, "Editable");
    let xml = document_xml_of(&out);

    // The bare paragraph may have no pPr at all; if it does, it must carry none
    // of the flags. Search the whole paragraph slice.
    let at = xml.find("Bare").expect("Bare present");
    let p_start = xml[..at]
        .rfind("<w:p>")
        .or_else(|| xml[..at].rfind("<w:p "))
        .unwrap();
    let p_end = xml[at..].find("</w:p>").map(|e| at + e).unwrap();
    let para = &xml[p_start..p_end];
    for frag in [
        "<w:bidi",
        "<w:mirrorIndents",
        "<w:wordWrap",
        "<w:overflowPunct",
        "<w:pageBreakBefore",
        "<w:snapToGrid",
        "<w:suppressAutoHyphens",
        "<w:adjustRightInd",
    ] {
        assert!(
            !para.contains(frag),
            "no flag may be materialized onto a paragraph that authored none: found {frag} in {para}"
        );
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Reject fixpoint: rejecting a tracked redline restores the untouched
// paragraph's numPr and flags verbatim.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn reject_restores_untouched_numpr_and_flags() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:pStyle w:val="Heading"/>
        <w:bidi w:val="0"/>
        <w:pageBreakBefore w:val="0"/>
      </w:pPr>
      <w:r><w:t>InheritedNumbered</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Editable</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_HEADING, NUMBERING_PSTYLE_BOUND);
    let doc = Document::parse(&docx).expect("parse");

    // Author a TRACKED edit on the OTHER paragraph, serialize the redline.
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

    let ppr = ppr_of_paragraph(&xml, "InheritedNumbered");
    assert!(
        !ppr.contains("<w:numPr"),
        "reject must leave the untouched inherited-numbering paragraph WITHOUT a \
         materialized direct numPr: {ppr}"
    );
    assert!(
        ppr.contains(r#"<w:bidi w:val="0""#) && ppr.contains(r#"<w:pageBreakBefore w:val="0""#),
        "reject must restore the untouched paragraph's explicit-off flags verbatim: {ppr}"
    );
    assert!(xml.contains("InheritedNumbered") && xml.contains("Editable"));
}

// ────────────────────────────────────────────────────────────────────────────
// Two-cycle idempotence for a flags+inherited-numbering paragraph.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn two_cycle_idempotence_of_numpr_and_flags() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:pStyle w:val="Heading"/>
        <w:bidi w:val="0"/>
        <w:wordWrap/>
      </w:pPr>
      <w:r><w:t>InheritedNumbered</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body), STYLES_HEADING, NUMBERING_PSTYLE_BOUND);

    let cycle = |bytes: &[u8]| -> Vec<u8> {
        Document::parse(bytes)
            .expect("parse")
            .serialize(&ExportOptions::default())
            .expect("serialize")
    };
    let once = cycle(&docx);
    let twice = cycle(&once);
    let ppr1 = ppr_of_paragraph(&document_xml_of(&once), "InheritedNumbered").to_string();
    let ppr2 = ppr_of_paragraph(&document_xml_of(&twice), "InheritedNumbered").to_string();
    assert_eq!(
        ppr1, ppr2,
        "the authored pPr (flags, no injected numPr) must be a fixpoint across a \
         second round-trip"
    );
    assert!(
        !ppr1.contains("<w:numPr")
            && ppr1.contains(r#"<w:bidi w:val="0""#)
            && ppr1.contains("<w:wordWrap"),
        "and it must be the faithful authored form: {ppr1}"
    );
}
