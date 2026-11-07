//! Torn range-marker repair under shapes with several pairs, nested tables
//! and mixed families sharing one id space.
//!
//! Domain rule (ECMA-376 §17.13.6): a range is a pair of positions, so a
//! resolution that removes the content holding one half keeps that half at
//! the removal point (`tracked_model::retain_segments_keeping_range_markers`);
//! zero-width markers that end up at one position keep their document order.
//! Three properties, each argued from that rule:
//!
//!  (a) Several pairs whose ends sat in the SAME removed insertion keep their
//!      ends at the removal point, in document order — no end is re-attached
//!      next to the wrong start.
//!  (b) A pair inside table cells nested within another table's cell is kept
//!      paired there, not hoisted out of the table.
//!  (c) Bookmark + comment-range + permission pairs torn in one document all
//!      stay paired, each within its own family — families that happen to
//!      share a numeric id space must not be confused.
//!
//! Daily tier: synthesized in-memory DOCX, no corpus, no real-Word oracle.

use std::io::{Read, Write};

use stemma::api::{Document, validate};
use stemma::{ExportOptions, Resolution};

fn pack(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        use zip::write::FileOptions;
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

fn reject_all_xml(body_inner: &str) -> (Vec<u8>, String) {
    let doc = Document::parse(&pack(body_inner)).expect("parse");
    let bytes = doc
        .project(Resolution::RejectAll)
        .expect("reject")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&bytes);
    (bytes, xml)
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).unwrap();
    let mut file = zip.by_name("word/document.xml").unwrap();
    let mut out = String::new();
    file.read_to_string(&mut out).unwrap();
    out
}

/// The ordered sequence of range-marker halves in the document, as
/// `(local-name, pairing-id)` pairs in document order. Enough to assert the
/// COLLAPSE POINT ordering, not just marker presence.
fn ordered_markers(xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt..];
        let end = rest.find('>').map(|e| e + 1).unwrap_or(rest.len());
        let tag = &rest[..end];
        for name in [
            "w:bookmarkStart",
            "w:bookmarkEnd",
            "w:commentRangeStart",
            "w:commentRangeEnd",
            "w:permStart",
            "w:permEnd",
        ] {
            if tag.starts_with(&format!("<{name}"))
                && let Some(idx) = tag.find(r#"w:id=""#)
            {
                let after = &tag[idx + 6..];
                let id: String = after.chars().take_while(|&c| c != '"').collect();
                out.push((name.trim_start_matches("w:").to_string(), id));
            }
        }
        rest = &rest[end..];
    }
    out
}

/// (a) Two bookmark pairs whose ends are both inside one rejected insertion, so
/// both surviving starts sit in the SAME base segment. Rejecting removes the
/// inserted text; both ends stay at the removal point, which is also where the
/// starts are, so each range becomes a point and the four markers keep their
/// document order:
///   start301, start302, end301, end302.
#[test]
fn two_pairs_collapsing_into_one_segment_stay_each_a_point() {
    let body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Alpha </w:t></w:r>
      <w:bookmarkStart w:id="301" w:name="_A"/>
      <w:bookmarkStart w:id="302" w:name="_B"/>
      <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>gone</w:t></w:r>
        <w:bookmarkEnd w:id="301"/>
        <w:bookmarkEnd w:id="302"/>
      </w:ins>
      <w:r><w:t xml:space="preserve"> Omega</w:t></w:r>
    </w:p>"#;
    let (bytes, xml) = reject_all_xml(body);

    let markers = ordered_markers(&xml);
    assert_eq!(
        markers,
        vec![
            ("bookmarkStart".to_string(), "301".to_string()),
            ("bookmarkStart".to_string(), "302".to_string()),
            ("bookmarkEnd".to_string(), "301".to_string()),
            ("bookmarkEnd".to_string(), "302".to_string()),
        ],
        "both ends stay at the removal point, in document order: {xml}"
    );
    assert!(
        !xml.contains("gone"),
        "the rejected insertion content is gone: {xml}"
    );
    assert!(
        xml.contains("Alpha") && xml.contains("Omega"),
        "surrounding base text survives: {xml}"
    );
    assert!(
        validate(&bytes).ok,
        "must validate clean: {:?}",
        validate(&bytes).issues
    );
}

/// (b) The surviving half sits inside a table nested within another table's cell.
/// The removed half must stay paired there — a pair left torn would make the
/// serializer refuse.
#[test]
fn survivor_inside_nested_table_cell_is_repaired() {
    let inner_para = r#"<w:p>
        <w:r><w:t xml:space="preserve">Cell </w:t></w:r>
        <w:bookmarkStart w:id="401" w:name="_C"/>
        <w:ins w:id="9" w:author="A" w:date="2024-01-01T00:00:00Z">
          <w:r><w:t>gone</w:t></w:r>
          <w:bookmarkEnd w:id="401"/>
        </w:ins>
        <w:r><w:t xml:space="preserve"> tail</w:t></w:r>
      </w:p>"#;
    let inner_table = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr>{inner_para}</w:tc></w:tr></w:tbl>"#
    );
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4675"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr>{inner_table}</w:tc></w:tr></w:tbl><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    let (bytes, xml) = reject_all_xml(&body);

    assert_eq!(
        xml.matches("<w:bookmarkStart").count(),
        1,
        "one surviving start: {xml}"
    );
    assert_eq!(
        xml.matches("<w:bookmarkEnd").count(),
        1,
        "the torn end is re-paired inside the nested cell: {xml}"
    );
    assert_eq!(
        xml.matches(r#"w:id="401""#).count(),
        2,
        "both halves keep the pairing id: {xml}"
    );
    // The re-paired end must land inside the cell (before the run bearing
    // " tail"), i.e. the collapse point is adjacent to the surviving start, not
    // hoisted out of the table.
    let start_at = xml.find("<w:bookmarkStart").unwrap();
    let end_at = xml.find("<w:bookmarkEnd").unwrap();
    let tail_at = xml.find(" tail").unwrap();
    assert!(
        start_at < end_at && end_at < tail_at,
        "collapse point sits at the surviving start inside the cell: {xml}"
    );
    assert!(
        !xml.contains("gone"),
        "the rejected insertion content is gone: {xml}"
    );
    assert!(
        validate(&bytes).ok,
        "must validate clean: {:?}",
        validate(&bytes).issues
    );
}

/// (c) A bookmark, a comment range and a permission range that share the SAME
/// numeric pairing id (600), all with their ends in the same rejected
/// insertion. Each end stays at the removal point within its own family; a
/// repair keyed on id alone would cross-pair them (or drop one).
#[test]
fn mixed_families_shared_id_do_not_cross_pair() {
    let body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Alpha </w:t></w:r>
      <w:bookmarkStart w:id="600" w:name="_BM"/>
      <w:commentRangeStart w:id="600"/>
      <w:permStart w:id="600" w:edGrp="everyone"/>
      <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>gone</w:t></w:r>
        <w:bookmarkEnd w:id="600"/>
        <w:commentRangeEnd w:id="600"/>
        <w:permEnd w:id="600"/>
      </w:ins>
      <w:r><w:t xml:space="preserve"> Omega</w:t></w:r>
    </w:p>"#;
    let (bytes, xml) = reject_all_xml(body);

    // Each family keeps one start and one end, despite sharing id 600.
    for (open, close) in [
        ("<w:bookmarkStart ", "<w:bookmarkEnd "),
        ("<w:commentRangeStart ", "<w:commentRangeEnd "),
        ("<w:permStart ", "<w:permEnd "),
    ] {
        assert_eq!(xml.matches(open).count(), 1, "one surviving {open}: {xml}");
        assert_eq!(
            xml.matches(close).count(),
            1,
            "the torn {close} is re-paired: {xml}"
        );
    }

    // The ends keep the document order they had in the insertion, after the
    // starts that sat before it.
    let markers = ordered_markers(&xml);
    assert_eq!(
        markers,
        vec![
            ("bookmarkStart".to_string(), "600".to_string()),
            ("commentRangeStart".to_string(), "600".to_string()),
            ("permStart".to_string(), "600".to_string()),
            ("bookmarkEnd".to_string(), "600".to_string()),
            ("commentRangeEnd".to_string(), "600".to_string()),
            ("permEnd".to_string(), "600".to_string()),
        ],
        "each end stays at the removal point within its own family: {xml}"
    );

    assert!(
        !xml.contains("gone"),
        "the rejected insertion content is gone: {xml}"
    );
    assert!(
        validate(&bytes).ok,
        "bookmark/permission balance validates clean (comment integrity is \
         advisory): {:?}",
        validate(&bytes).issues
    );
}
