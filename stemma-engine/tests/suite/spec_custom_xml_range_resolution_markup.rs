//! Markup-level accept/reject resolution for customXml*Range-governed wrappers.
//!
//! The text-reading constraints live in
//! `spec_custom_xml_smart_tags_word_compliance.rs` (and pass after the
//! transparent-wrapper fix). This file pins the *markup* Word emits
//! when accepting / rejecting a `customXml`/`smartTag` wrapper that is governed
//! by a `customXml*Range` revision-marker pair — a constraint the text-reading
//! tests cannot distinguish (the text is identical whether or not the wrapper
//! survives).
//!
//! Source of truth: the accept/reject `document.xml` real Word produces for
//! these repros. Word's rule, read off those outputs:
//!   * A wrapper GOVERNED by a `customXmlInsRange` / `customXmlDelRange` /
//!     `customXmlMove*Range` pair is REMOVED — along with the range markers —
//!     on BOTH accept and reject. The range marks the *wrapper markup itself*
//!     as the revision, so resolving it either way drops the transient wrapper;
//!     only the inner text survives (or is deleted by an inner `w:del`).
//!   * A PLAIN wrapper (no governing range marker) is KEPT verbatim on both
//!     accept and reject (covered in the sibling file's
//!     `smarttag_inline_content_transparent_in_text`, asserting opens-clean +
//!     text; the wrapper-kept markup is the verbatim-passthrough contract).
//!
//! Fixed by `resolve_custom_xml_range_governed_wrappers`
//! (tracked_model.rs), which runs in the accept/reject projection: a `customXml*Range`
//! marker pair and the wrapper it encloses are dropped on both resolutions,
//! leaving the inner content. The expected markup is read directly from the
//! output real Word saves.

use std::io::{Read, Write};

use stemma::ExportOptions;
use stemma::api::Document;

fn make_docx(body_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_xml}</w:body></w:document>"#
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

fn document_xml_of(docx: &[u8]) -> String {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("open docx zip");
    let mut file = archive
        .by_name("word/document.xml")
        .expect("word/document.xml present");
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read document.xml");
    s
}

/// Accept-all, then serialize, and return the resulting `word/document.xml`.
fn accepted_xml(bytes: &[u8]) -> String {
    let doc = Document::parse(bytes).expect("parse");
    let out = doc
        .read_accepted()
        .expect("read_accepted")
        .serialize(&ExportOptions::default())
        .expect("serialize accepted");
    document_xml_of(&out)
}

/// Reject-all, then serialize, and return the resulting `word/document.xml`.
fn rejected_xml(bytes: &[u8]) -> String {
    let doc = Document::parse(bytes).expect("parse");
    let out = doc
        .read_rejected()
        .expect("read_rejected")
        .serialize(&ExportOptions::default())
        .expect("serialize rejected");
    document_xml_of(&out)
}

/// Rule: accepting OR rejecting a `customXmlInsRange`-governed wrapper removes
/// the wrapper element AND the range markers, leaving only the bare run text.
/// (ECMA §17.13.5.6/.7; accepting and rejecting in Word both emit
/// `<w:r><w:t>Tristan Davis signed.</w:t></w:r>` with no customXml, no markers.)
#[test]
fn customxml_ins_range_accept_and_reject_drop_wrapper_and_markers() {
    let body = r#"<w:p><w:customXmlInsRangeStart w:id="1" w:author="Reviewer" w:date="2024-01-01T00:00:00Z"/><w:customXml w:uri="urn:stemma:test" w:element="customerName"><w:r><w:t>Tristan Davis</w:t></w:r></w:customXml><w:customXmlInsRangeEnd w:id="1"/><w:r><w:t xml:space="preserve"> signed.</w:t></w:r></w:p><w:sectPr/>"#;
    let b = make_docx(body);

    for (label, xml) in [("accept", accepted_xml(&b)), ("reject", rejected_xml(&b))] {
        assert!(
            xml.contains("Tristan Davis"),
            "{label}: the wrapped run text is ordinary document text and must survive \
             (§17.13.5.7). Got: {xml}"
        );
        assert!(
            !xml.contains("customXml"),
            "{label}: §17.13.5.6/.7 — the customXmlInsRange marks the WRAPPER markup as the \
             revision; Word removes the customXml wrapper AND the customXmlInsRange markers on \
             both accept and reject, leaving bare runs. Got: {xml}"
        );
    }
}

/// Rule: a `customXmlDelRange`-governed wrapper with NO inner `w:del` keeps the
/// text on both accept and reject, but the wrapper + range markers are removed.
/// (ECMA §17.13.5.4/.5, MS-OE376 §2.1.326; Word oracle both emit
/// `<w:r><w:t>Keep this text.</w:t></w:r>`.)
#[test]
fn customxml_del_range_no_inner_del_drops_wrapper_keeps_text() {
    let body = r#"<w:p><w:customXmlDelRangeStart w:id="2" w:author="Reviewer" w:date="2024-01-01T00:00:00Z"/><w:customXml w:uri="urn:stemma:test" w:element="invoice"><w:r><w:t>Keep this text.</w:t></w:r></w:customXml><w:customXmlDelRangeEnd w:id="2"/></w:p><w:sectPr/>"#;
    let b = make_docx(body);

    for (label, xml) in [("accept", accepted_xml(&b)), ("reject", rejected_xml(&b))] {
        assert!(
            xml.contains("Keep this text."),
            "{label}: with no inner w:del, only the wrapper markup is the revision; the text must \
             survive (§17.13.5.5, MS-OE376 §2.1.326). Got: {xml}"
        );
        assert!(
            !xml.contains("customXml"),
            "{label}: Word removes the customXml wrapper AND the customXmlDelRange markers on both \
             accept and reject. Got: {xml}"
        );
    }
}

/// Rule: a `customXmlDelRange`-governed wrapper WITH an inner `w:del` —
/// accepting removes the wrapper, the range markers, AND the deleted text;
/// rejecting removes the wrapper + markers but restores the deleted text as a
/// plain run. (ECMA §17.13.5.5/.14; Word oracle accept → `Keep ` + `.`,
/// reject → `Keep ` + `Gone` + `.`, no customXml in either.)
#[test]
fn customxml_del_range_with_inner_del_resolves_and_drops_wrapper() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:customXmlDelRangeStart w:id="1" w:author="Miner" w:date="2026-01-01T00:00:00Z"/><w:customXml w:element="note"><w:del w:id="2" w:author="Miner" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>Gone</w:delText></w:r></w:del></w:customXml><w:customXmlDelRangeEnd w:id="1"/><w:r><w:t>.</w:t></w:r></w:p><w:sectPr/>"#;
    let b = make_docx(body);

    let accept = accepted_xml(&b);
    assert!(
        !accept.contains("customXml"),
        "accept: Word drops the customXml wrapper + customXmlDelRange markers. Got: {accept}"
    );
    assert!(
        !accept.contains("Gone"),
        "accept: the inner w:del deletes 'Gone' (§17.13.5.14). Got: {accept}"
    );

    let reject = rejected_xml(&b);
    assert!(
        !reject.contains("customXml"),
        "reject: Word drops the customXml wrapper + customXmlDelRange markers. Got: {reject}"
    );
    assert!(
        reject.contains("Gone"),
        "reject: rejecting the inner w:del restores 'Gone' as a plain run (§17.13.5.5). Got: \
         {reject}"
    );
}

/// Rule: a `customXmlMove*Range`-governed pair of wrappers resolves by the
/// inner moveFrom/moveTo, and BOTH the customXml wrappers and the
/// customXmlMove*Range markers are removed on accept and reject.
/// (ECMA §17.13.5.8–.11/.22/.25; Word oracle accept → `A`+`C`+`B`, reject →
/// `A`+`B`+`C`, no customXml in either.)
#[test]
fn customxml_move_range_resolves_and_drops_wrappers() {
    let body = r#"<w:p><w:r><w:t>A</w:t></w:r><w:moveFromRangeStart w:id="1" w:name="move1" w:author="A" w:date="2026-01-01T00:00:00Z"/><w:customXmlMoveFromRangeStart w:id="2"/><w:customXml w:uri="urn:stemma:frag" w:element="frag"><w:moveFrom w:id="3" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>B</w:t></w:r></w:moveFrom></w:customXml><w:customXmlMoveFromRangeEnd w:id="2"/><w:moveFromRangeEnd w:id="1"/><w:r><w:t>C</w:t></w:r><w:moveToRangeStart w:id="4" w:name="move1" w:author="A" w:date="2026-01-01T00:00:00Z"/><w:customXmlMoveToRangeStart w:id="5"/><w:customXml w:uri="urn:stemma:frag" w:element="frag"><w:moveTo w:id="6" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>B</w:t></w:r></w:moveTo></w:customXml><w:customXmlMoveToRangeEnd w:id="5"/><w:moveToRangeEnd w:id="4"/></w:p><w:sectPr/>"#;
    let b = make_docx(body);

    for (label, xml) in [("accept", accepted_xml(&b)), ("reject", rejected_xml(&b))] {
        assert!(
            !xml.contains("customXml"),
            "{label}: §17.13.5.8–.11 — the customXmlMove*Range markers track only the wrapper; \
             Word removes both customXml wrappers and the customXmlMove*Range markers on accept \
             and reject. Got: {xml}"
        );
    }
}
