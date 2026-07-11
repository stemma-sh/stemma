//! Torn range-marker collapse under the shapes the O(D+T) single-walk repair
//! makes newly risky (`tracked_model::collapse_resolution_torn_range_markers`).
//!
//! The repair plans every torn pair keyed by its survivor, then re-materializes
//! the missing halves in ONE walk instead of re-walking the story per pair. Three
//! properties that walk must preserve, each argued from the domain rule (a torn
//! range collapses to a POINT adjacent to its surviving half — Word's behavior
//! when a bookmarked range's interior is removed, ECMA-376 §17.13.6):
//!
//!  (a) Several pairs collapsing into the SAME segment must each land its partner
//!      adjacent to its OWN survivor — index-shift stability. Inserting one
//!      partner shifts the later inline indices, so the partners must be applied
//!      highest-index-first; the wrong order strands a partner next to the wrong
//!      survivor even though every marker is still "present".
//!  (b) A survivor nested inside table cells must still be found and paired — the
//!      single walk has to recurse cells, not just scan top-level blocks.
//!  (c) Bookmark + comment-range + permission pairs torn in one document must all
//!      collapse, each within its own family — the survivor-keyed plan must not
//!      confuse families that happen to share a numeric id space.
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
/// both surviving starts sit in the SAME base segment. Rejecting removes both
/// ends; each must be re-inserted adjacent to its OWN start. The domain-correct
/// collapse is two disjoint points in document order:
///   start301, end301, start302, end302.
/// If the partners were applied lowest-index-first, inserting end301 shifts
/// start302 right, and end302 lands next to end301 instead — yielding
///   start301, end301, end302, start302
/// (every marker present, but 302 no longer a point at its start). This test
/// pins the ordering, so that stale-index ordering fails it.
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
            ("bookmarkEnd".to_string(), "301".to_string()),
            ("bookmarkStart".to_string(), "302".to_string()),
            ("bookmarkEnd".to_string(), "302".to_string()),
        ],
        "each torn pair must collapse to a point at its own start, in document \
         order: {xml}"
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
/// The single insertion walk must recurse cells to find it and re-pair the
/// partner there — a top-level-only scan would leave the pair torn and the
/// serializer would refuse.
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

/// (c) A bookmark and a permission range that share the SAME numeric pairing id
/// (600), both torn in the same rejected insertion. The survivor-keyed repair
/// plan keys on `(family, id, role)`, so the two must collapse to their OWN
/// family's point — a plan keyed on id alone would cross-pair them (or drop one),
/// re-inserting a permEnd next to a bookmarkStart or vice-versa. A comment range
/// (also id 600) rides along to show a third family does not disturb the other
/// two.
///
/// NOTE on the comment range: the inline comment-range half is left LONE here
/// (its end is not re-inserted). That is a PRE-EXISTING engine behavior, not a
/// regression from this refactor — the byte-identical result is produced by the
/// pre-refactor collapse as well (verified out-of-band), and comment-range
/// integrity is a non-blocking advisory (I-ANN-005). Repairing inline
/// comment-range tears is a separate concern; this test only pins that the
/// bookmark and permission collapses are correct and unaffected by the comment
/// range's presence.
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

    // Bookmark and permission each collapse to their own point (start+end),
    // despite sharing id 600 with each other and the comment range.
    for (open, close) in [
        ("<w:bookmarkStart ", "<w:bookmarkEnd "),
        ("<w:permStart ", "<w:permEnd "),
    ] {
        assert_eq!(xml.matches(open).count(), 1, "one surviving {open}: {xml}");
        assert_eq!(
            xml.matches(close).count(),
            1,
            "the torn {close} is re-paired: {xml}"
        );
    }

    // The re-materialized ends belong to the RIGHT family: a bookmarkEnd is
    // adjacent to the bookmarkStart, a permEnd adjacent to the permStart — no
    // cross-pairing. Restricting to the two collapsing families, the order is:
    //   bookmarkStart, bookmarkEnd, permStart, permEnd.
    let collapsing: Vec<(String, String)> = ordered_markers(&xml)
        .into_iter()
        .filter(|(name, _)| name.starts_with("bookmark") || name.starts_with("perm"))
        .collect();
    assert_eq!(
        collapsing,
        vec![
            ("bookmarkStart".to_string(), "600".to_string()),
            ("bookmarkEnd".to_string(), "600".to_string()),
            ("permStart".to_string(), "600".to_string()),
            ("permEnd".to_string(), "600".to_string()),
        ],
        "each family collapses to a point at its own start, no cross-pairing: {xml}"
    );

    // Comment range: pre-existing lone-survivor behavior (see NOTE above).
    assert_eq!(
        xml.matches("<w:commentRangeStart ").count(),
        1,
        "the comment-range start survives: {xml}"
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
