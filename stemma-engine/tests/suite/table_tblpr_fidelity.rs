//! RFC-0003 import fidelity: `tblPr` children that used to be silently dropped
//! (table-level `w:shd`, `w:bidiVisual`, `w:tblCaption`, `w:tblDescription`) now
//! round-trip through parse → serialize, and ANY unmodeled `tblPr` child (vendor
//! extensions in a foreign namespace) is preserved verbatim rather than dropped.
//!
//! Methodology: stemma reconstructs tables from the typed model
//! (`serialize_table_node`), so a `tblPr` property the model does not carry is
//! genuinely dropped on save. These tests assert the property SURVIVES a full
//! parse → serialize round-trip and the output still opens clean.

use std::io::{Cursor, Read, Write};

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::{Document, validate};
use stemma::edit::*;
use zip::ZipWriter;
use zip::write::FileOptions;

fn make_docx(body_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_xml}</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = FileOptions::default();
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

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx)).expect("open docx zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("document.xml present");
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read document.xml");
    s
}

fn reserialize(bytes: &[u8]) -> String {
    let doc = Document::parse(bytes).expect("parse");
    let out = doc.serialize(&ExportOptions::default()).expect("serialize");
    document_xml_of(&out)
}

fn one_cell_table(tbl_pr_inner: &str) -> String {
    format!(
        r#"<w:tbl><w:tblPr>{tbl_pr_inner}</w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>"#
    )
}

#[test]
fn table_level_shd_round_trips() {
    let body = one_cell_table(
        r#"<w:tblW w:w="5000" w:type="pct"/><w:shd w:val="clear" w:color="auto" w:fill="D9D9D9"/>"#,
    );
    let out = reserialize(&make_docx(&body));
    assert!(
        out.contains("w:fill=\"D9D9D9\""),
        "table-level w:shd fill must survive round-trip; got: {out}"
    );
    assert!(validate(&make_docx(&body)).ok, "output opens clean");
}

#[test]
fn bidi_visual_round_trips() {
    let body = one_cell_table(r#"<w:tblW w:w="5000" w:type="pct"/><w:bidiVisual/>"#);
    let out = reserialize(&make_docx(&body));
    assert!(
        out.contains("<w:bidiVisual"),
        "w:bidiVisual must survive round-trip; got: {out}"
    );
}

#[test]
fn tbl_caption_and_description_round_trip() {
    let body = one_cell_table(
        r#"<w:tblW w:w="5000" w:type="pct"/><w:tblCaption w:val="My Caption"/><w:tblDescription w:val="My Description"/>"#,
    );
    let out = reserialize(&make_docx(&body));
    assert!(
        out.contains("w:val=\"My Caption\""),
        "w:tblCaption must survive round-trip; got: {out}"
    );
    assert!(
        out.contains("w:val=\"My Description\""),
        "w:tblDescription must survive round-trip; got: {out}"
    );
}

#[test]
fn unmodeled_vendor_child_is_preserved_not_dropped() {
    // A foreign-namespace tblPr child the typed model does not model. RFC-0003
    // "never silently drop": it must be captured and re-emitted verbatim.
    let body = one_cell_table(
        r#"<w:tblW w:w="5000" w:type="pct"/><o:tabOrder xmlns:o="urn:schemas-microsoft-com:office:office" o:val="7"/>"#,
    );
    let out = reserialize(&make_docx(&body));
    assert!(
        out.contains("tabOrder"),
        "an unmodeled vendor tblPr child must be preserved, not dropped; got: {out}"
    );
}

#[test]
fn all_new_tblpr_properties_together_open_clean() {
    let body = one_cell_table(
        r#"<w:tblStyle w:val="TableGrid"/><w:tblW w:w="5000" w:type="pct"/><w:shd w:val="clear" w:color="auto" w:fill="EEECE1"/><w:tblCaption w:val="C"/><w:tblDescription w:val="D"/>"#,
    );
    let docx = make_docx(&body);
    assert!(
        validate(&docx).ok,
        "combined tblPr properties open clean: {:?}",
        validate(&docx).issues
    );
    let out = reserialize(&docx);
    // Schema order (CT_TblPr): tblStyle → tblW → tblBorders → shd → … → tblCaption → tblDescription.
    let shd_i = out.find("<w:shd").expect("shd present");
    let cap_i = out.find("w:val=\"C\"").expect("caption present");
    assert!(
        shd_i < cap_i,
        "shd must precede tblCaption in CT_TblPr order"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// w:tblpPr (§17.4.58 CT_TblPPr) — the FULL floating-table positioning attribute
// set. The model used to carry only vertAnchor/horzAnchor/tblpX/tblpY, so a
// rebuild silently dropped the text clearances (leftFromText/rightFromText/
// topFromText/bottomFromText) and the relative alignment specs (tblpXSpec/
// tblpYSpec). The wild footer table
//   <w:tblpPr w:leftFromText="187" w:rightFromText="187" w:vertAnchor="page"
//             w:horzAnchor="margin" w:tblpXSpec="center" w:tblpYSpec="bottom"/>
// re-emitted as <w:tblpPr w:vertAnchor="page" w:horzAnchor="margin"/> — the
// table lost its centered/bottom page position and its text clearances, a
// layout-visible regression. These pin the full set.
// ────────────────────────────────────────────────────────────────────────────

/// The complete CT_TblPPr attribute surface (§17.4.58): the two anchors, the two
/// absolute offsets, the four distance-from-text clearances, and the two
/// relative alignment specs.
const FULL_TBLPPR: &str = r#"<w:tblpPr w:leftFromText="187" w:rightFromText="188" w:topFromText="45" w:bottomFromText="46" w:vertAnchor="page" w:horzAnchor="margin" w:tblpX="720" w:tblpXSpec="center" w:tblpY="1440" w:tblpYSpec="bottom"/>"#;

/// Every CT_TblPPr attribute paired with its authored value, for `contains`
/// assertions.
const FULL_TBLPPR_PAIRS: &[(&str, &str)] = &[
    ("w:leftFromText", "187"),
    ("w:rightFromText", "188"),
    ("w:topFromText", "45"),
    ("w:bottomFromText", "46"),
    ("w:vertAnchor", "page"),
    ("w:horzAnchor", "margin"),
    ("w:tblpX", "720"),
    ("w:tblpXSpec", "center"),
    ("w:tblpY", "1440"),
    ("w:tblpYSpec", "bottom"),
];

/// Apply an unrelated tracked edit to the body paragraph carrying `target_text`,
/// forcing a whole-document rebuild, and return the serialized bytes. Mirrors
/// `edit_a_different_paragraph` from `spec_untouched_para_formatting_roundtrip`.
fn edit_body_paragraph(doc: &Document, target_text: &str) -> Vec<u8> {
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
        summary: Some("unrelated body edit forcing a rebuild".to_string()),
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Spec".to_string()),
            date: Some("2026-07-10T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    doc.apply(&txn)
        .expect("apply unrelated edit")
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

#[test]
fn full_tblppr_attribute_set_survives_rebuild() {
    // A floating body table carrying the full CT_TblPPr surface, plus a separate
    // editable paragraph. An edit to the paragraph forces a whole-document
    // rebuild; every tblpPr attribute must survive.
    let body = format!(
        r#"<w:p><w:r><w:t>Editable</w:t></w:r></w:p><w:tbl><w:tblPr>{FULL_TBLPPR}<w:tblW w:w="5000" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>"#
    );
    let docx = make_docx(&body);
    let doc = Document::parse(&docx).expect("parse");
    let out = document_xml_of(&edit_body_paragraph(&doc, "Editable"));

    for (attr, value) in FULL_TBLPPR_PAIRS {
        assert!(
            out.contains(&format!(r#"{attr}="{value}""#)),
            "§17.4.58: tblpPr {attr}=\"{value}\" must survive a whole-document \
             rebuild (was silently dropped before the full attribute set was \
             modeled): {out}"
        );
    }
    assert!(validate(&docx).ok, "input opens clean");
}

#[test]
fn footer_floating_table_full_tblppr_survives_body_edit() {
    // The wild witness: the floating table lives in a FOOTER. An unrelated
    // tracked edit to the BODY forces the footer story to reserialize from the
    // typed model; every CT_TblPPr attribute must survive into footer1.xml.
    let footer_table = format!(
        r#"<w:tbl><w:tblPr>{FULL_TBLPPR}<w:tblW w:w="5000" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>footer cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
    );
    let docx = make_footer_docx(&footer_table);
    let doc = Document::parse(&docx).expect("parse");
    let out = edit_body_paragraph(&doc, "Body paragraph");
    let footer = part_of(&out, "word/footer1.xml");

    for (attr, value) in FULL_TBLPPR_PAIRS {
        assert!(
            footer.contains(&format!(r#"{attr}="{value}""#)),
            "§17.4.58: footer floating-table tblpPr {attr}=\"{value}\" must survive \
             a body edit that reserializes the footer story: {footer}"
        );
    }
}

/// Read an arbitrary part out of a DOCX zip.
fn part_of(docx: &[u8], part: &str) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx)).expect("open docx zip");
    let mut file = zip
        .by_name(part)
        .unwrap_or_else(|_| panic!("{part} present"));
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read part");
    s
}

/// Build a DOCX with one body paragraph, a `footer1.xml` part whose content is
/// `footer_inner`, and a `footerReference` in the body sectPr. Mirrors
/// `make_footer_docx` from `blindspot_editfooter_story.rs`.
fn make_footer_docx(footer_inner: &str) -> Vec<u8> {
    let footer_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{footer_inner}</w:ftr>"#
    );
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>
<w:sectPr>
<w:footerReference w:type="default" r:id="rIdF1"/>
<w:pgSz w:w="12240" w:h="15840"/>
</w:sectPr>
</w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;

    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.start_file("word/footer1.xml", opts).unwrap();
        zip.write_all(footer_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}
