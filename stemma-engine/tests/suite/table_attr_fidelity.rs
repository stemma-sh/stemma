//! RFC-0003 attribute-level fidelity: theme colors, frame, and shadow on table
//! borders/shading are ATTRIBUTES on a MODELED element (`w:tcBorders`, `w:shd`).
//! The element is parsed, but its unmodeled attributes used to be dropped on
//! save. They now round-trip verbatim via `Border.extra_attrs` / `Shading.extra_attrs`.

use std::io::{Cursor, Read, Write};

use stemma::ExportOptions;
use stemma::api::{Document, validate};
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

/// A one-cell table whose single cell carries `tc_pr_inner`.
fn table_with_tc_pr(tc_pr_inner: &str) -> String {
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/>{tc_pr_inner}</w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>"#
    )
}

#[test]
fn cell_border_theme_color_round_trips() {
    let body = table_with_tc_pr(
        r#"<w:tcBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="4472C4" w:themeColor="accent1" w:themeTint="99"/></w:tcBorders>"#,
    );
    let out = reserialize(&make_docx(&body));
    assert!(
        out.contains(r#"w:themeColor="accent1""#),
        "cell border themeColor must round-trip; got: {out}"
    );
    assert!(
        out.contains(r#"w:themeTint="99""#),
        "cell border themeTint must round-trip; got: {out}"
    );
    assert!(validate(&make_docx(&body)).ok, "output opens clean");
}

#[test]
fn cell_shading_theme_fill_round_trips() {
    let body = table_with_tc_pr(
        r#"<w:shd w:val="clear" w:color="auto" w:fill="D9E2F3" w:themeFill="accent1" w:themeFillTint="33"/>"#,
    );
    let out = reserialize(&make_docx(&body));
    assert!(
        out.contains(r#"w:themeFill="accent1""#),
        "cell shading themeFill must round-trip; got: {out}"
    );
    assert!(
        out.contains(r#"w:themeFillTint="33""#),
        "cell shading themeFillTint must round-trip; got: {out}"
    );
}

#[test]
fn cell_border_frame_shadow_round_trip() {
    let body = table_with_tc_pr(
        r#"<w:tcBorders><w:top w:val="single" w:sz="4" w:color="000000" w:frame="true" w:shadow="true"/></w:tcBorders>"#,
    );
    let out = reserialize(&make_docx(&body));
    assert!(
        out.contains(r#"w:frame="true""#) && out.contains(r#"w:shadow="true""#),
        "cell border frame/shadow must round-trip; got: {out}"
    );
}

/// Return the substring of a cell's `w:tcBorders`, so an assertion on the
/// cell's edges is not confused by the table-level `w:tblBorders`.
fn extract_tc_borders(xml: &str) -> &str {
    let start = xml.find("<w:tcBorders").expect("tcBorders present");
    let rest = &xml[start..];
    let end = rest.find("</w:tcBorders>").expect("tcBorders closes");
    &rest[..end]
}

/// §17.4.39: an absent `w:tcBorders` edge is a deliberate authoring choice —
/// the edge defers to the table-level border / adjacent-cell resolution and
/// must NOT be re-emitted as a materialized line. Import resolves an EFFECTIVE
/// top edge (from the table border) into the cell model for projections, but
/// the serializer must emit only the AUTHORED edges. A cell that authored
/// left/bottom/right only (top deliberately absent) must round-trip — even
/// across an unrelated body edit — with NO `w:top` element in its `tcBorders`.
///
/// Regression: the table rebuild used to serialize the resolved effective set,
/// synthesizing a `w:top` the author never wrote; Word then draws a phantom
/// line and reconciles the adjacent-cell conflict by minting a SYSTEM revision,
/// so the document still shows a tracked change after accept-all.
#[test]
fn cell_absent_top_border_edge_is_not_synthesized() {
    // A cell authoring left/bottom/right only, under a table whose tblBorders
    // carries every edge (so resolution HAS a top edge to cascade into the cell).
    // A separate body paragraph gives the unrelated edit a target outside the
    // table, forcing a full model reserialize of the untouched table.
    let cell_tc_borders = r#"<w:tcBorders><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tcBorders>"#;
    let table = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/>{cell_tc_borders}</w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
    );
    let body_before = format!(r#"<w:p><w:r><w:t>intro</w:t></w:r></w:p>{table}<w:sectPr/>"#);
    let body_after = format!(r#"<w:p><w:r><w:t>introEDIT</w:t></w:r></w:p>{table}<w:sectPr/>"#);

    let base = Document::parse(&make_docx(&body_before)).expect("parse base");
    let target = Document::parse(&make_docx(&body_after)).expect("parse target");
    let redline = base.diff_as(&target, "Reviewer").expect("diff_as");
    let out_bytes = redline
        .serialize(&ExportOptions::default())
        .expect("serialize redline");
    let out = document_xml_of(&out_bytes);

    let tc_borders = extract_tc_borders(&out);
    assert!(
        !tc_borders.contains("<w:top"),
        "authored-absent top edge must not be synthesized into tcBorders; got tcBorders: {tc_borders}"
    );
    // The authored edges must survive untouched.
    assert!(
        tc_borders.contains("<w:left")
            && tc_borders.contains("<w:bottom")
            && tc_borders.contains("<w:right"),
        "authored left/bottom/right edges must round-trip; got tcBorders: {tc_borders}"
    );
    assert!(validate(&out_bytes).ok, "redlined output opens clean");
}
