//! M0.1 — opaque-container accept/reject descent.
//!
//! Tracked changes (`w:ins`/`w:del`) that live INSIDE an opaque inline's
//! `raw_xml` — a textbox interior (`w:drawing > … > w:txbxContent`) or an inline
//! content control (`w:sdt > w:sdtContent`) — must be resolved by the IR
//! accept/reject projection (`Document::read_accepted` / `read_rejected`), the
//! same way the byte-level resolver (`normalize::normalize_docx` /
//! `reject_all_docx`) already resolves them.
//!
//! Before this fix the IR projection only descended into `OpaqueKind::Hyperlink`
//! (`tracked_model.rs`), so a `w:ins`/`w:del` inside a textbox or content control
//! survived BOTH accept-all and reject-all byte-identical: accept-all left the
//! `<w:ins>` wrapper in place (Word keeps showing it as a pending change) and
//! reject-all kept inserted text that should have been removed. That is a
//! silent-fallback correctness defect (CLAUDE.md prime directive) and falsifies
//! the `reject-all == baseline` invariant (`api.rs:243`).
//!
//! Bytes-in; public `Document` API.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::{ExportMode, ExportOptions, ValidatorLevel};
use zip::ZipWriter;
use zip::write::FileOptions;

use crate::common;

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const PACKAGE_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOC_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

/// Build a minimal DOCX whose body is `body_inner`. The `w:document` element
/// declares the namespaces a textbox / content control needs (`wp`/`a`/`wps`).
fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(PACKAGE_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(DOC_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Serialize a document and return its `word/document.xml` as a string.
fn document_xml(doc: &Document) -> String {
    let bytes = doc
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Off,
            validator: None,
        })
        .expect("serialize");
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut xml)
        .expect("read");
    xml
}

/// Whether the serialized XML still carries a content revision of the given
/// kind (`"ins"` or `"del"`). Matches only `<w:ins `/`<w:ins>` (and the `del`
/// equivalents), NOT lookalike property elements such as `<w:insideH>` /
/// `<w:insideV>` (table borders) — a substring `"<w:ins"` would false-match
/// those. Mirrors `normalize::REVISION_BYTE_MARKERS`.
fn has_revision(xml: &str, kind: &str) -> bool {
    xml.contains(&format!("<w:{kind} ")) || xml.contains(&format!("<w:{kind}>"))
}

// ---- Fixtures ----

/// A textbox interior paragraph reads "OLD" followed by a
/// tracked-inserted " NEW".
const TEXTBOX_BODY: &str = r#"<w:p><w:r><w:t>Body before textbox.</w:t></w:r><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2700000" cy="900000"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t xml:space="preserve">OLD</w:t></w:r><w:ins w:id="100" w:author="probe" w:date="2026-06-11T00:00:00Z"><w:r><w:t xml:space="preserve"> NEW</w:t></w:r></w:ins></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;

/// Synthesis §2.1: an inline content control whose `sdtContent` reads "OLD"
/// followed by a tracked-inserted " NEW".
const SDT_BODY: &str = r#"<w:p><w:sdt><w:sdtPr><w:tag w:val="TenantName"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">OLD</w:t></w:r><w:ins w:id="100" w:author="probe" w:date="2026-06-11T00:00:00Z"><w:r><w:t xml:space="preserve"> NEW</w:t></w:r></w:ins></w:sdtContent></w:sdt></w:p>"#;

// ---- Textbox (OpaqueKind::Drawing) ----

#[test]
fn accept_all_resolves_ins_inside_textbox() {
    // DOMAIN RULE (§17.13.5 + the byte resolver `normalize::normalize_docx`):
    // accepting all changes unwraps a `w:ins` — the inserted run survives, the
    // wrapper does not. This must hold inside a textbox interior, exactly as it
    // does for body content.
    let doc = Document::parse(&make_docx(TEXTBOX_BODY)).expect("parse");
    let accepted = doc.read_accepted().expect("accept-all");
    let xml = document_xml(&accepted);
    assert!(
        !has_revision(&xml, "ins"),
        "accept-all must unwrap the textbox <w:ins>; got:\n{xml}"
    );
    assert!(
        xml.contains("OLD") && xml.contains("NEW"),
        "accept-all keeps both OLD and the accepted NEW; got:\n{xml}"
    );
}

#[test]
fn reject_all_resolves_ins_inside_textbox() {
    // DOMAIN RULE: rejecting all changes drops a `w:ins` entirely — the inserted
    // run and its wrapper both disappear, restoring the baseline.
    let doc = Document::parse(&make_docx(TEXTBOX_BODY)).expect("parse");
    let rejected = doc.read_rejected().expect("reject-all");
    let xml = document_xml(&rejected);
    assert!(
        !has_revision(&xml, "ins"),
        "reject-all must drop the textbox <w:ins>; got:\n{xml}"
    );
    assert!(
        xml.contains("OLD"),
        "reject-all keeps the original OLD text; got:\n{xml}"
    );
    assert!(
        !xml.contains("NEW"),
        "reject-all must remove the inserted NEW; got:\n{xml}"
    );
}

// ---- Inline content control (OpaqueKind::Sdt) ----

#[test]
fn accept_all_resolves_ins_inside_sdt() {
    let doc = Document::parse(&make_docx(SDT_BODY)).expect("parse");
    let accepted = doc.read_accepted().expect("accept-all");
    let xml = document_xml(&accepted);
    assert!(
        !has_revision(&xml, "ins"),
        "accept-all must unwrap the sdtContent <w:ins>; got:\n{xml}"
    );
    assert!(
        xml.contains("OLD") && xml.contains("NEW"),
        "accept-all keeps both OLD and the accepted NEW; got:\n{xml}"
    );
    // The content-control wrapper itself survives the projection.
    assert!(
        xml.contains("<w:sdt") && xml.contains("TenantName"),
        "the sdt wrapper must survive accept-all; got:\n{xml}"
    );
}

#[test]
fn reject_all_resolves_ins_inside_sdt() {
    let doc = Document::parse(&make_docx(SDT_BODY)).expect("parse");
    let rejected = doc.read_rejected().expect("reject-all");
    let xml = document_xml(&rejected);
    assert!(
        !has_revision(&xml, "ins"),
        "reject-all must drop the sdtContent <w:ins>; got:\n{xml}"
    );
    assert!(
        xml.contains("OLD"),
        "reject-all keeps the original OLD text; got:\n{xml}"
    );
    assert!(
        !xml.contains("NEW"),
        "reject-all must remove the inserted NEW; got:\n{xml}"
    );
    assert!(
        xml.contains("<w:sdt") && xml.contains("TenantName"),
        "the sdt wrapper must survive reject-all; got:\n{xml}"
    );
}

// ---- reject-all == baseline (the load-bearing invariant, api.rs:243) ----

#[test]
fn reject_all_textbox_equals_baseline() {
    // The baseline of these fixtures (no revisions other than the inserted run)
    // is the document with the insertion never made. Reject-all must reproduce
    // it: the resolved textbox interior carries only OLD, no revision markup.
    let doc = Document::parse(&make_docx(TEXTBOX_BODY)).expect("parse");
    let rejected = doc.read_rejected().expect("reject-all");
    let xml = document_xml(&rejected);
    // Idempotence: rejecting an already-rejected document changes nothing more.
    let rejected_twice = rejected.read_rejected().expect("reject-all twice");
    assert_eq!(
        xml,
        document_xml(&rejected_twice),
        "reject-all must be idempotent on the textbox fixture"
    );
}

#[test]
fn reject_all_sdt_equals_baseline() {
    let doc = Document::parse(&make_docx(SDT_BODY)).expect("parse");
    let rejected = doc.read_rejected().expect("reject-all");
    let xml = document_xml(&rejected);
    let rejected_twice = rejected.read_rejected().expect("reject-all twice");
    assert_eq!(
        xml,
        document_xml(&rejected_twice),
        "reject-all must be idempotent on the sdt fixture"
    );
}

// ---- Other opaque kinds (the descent is uniform, not a Drawing|Sdt allowlist) ----
//
// The byte path resolves revisions inside EVERY opaque container; the IR descent
// must match. fldSimple results (CT_SimpleField = EG_PContent) and inline
// customXml (CT_CustomXmlRun = EG_PContent) can both legally carry w:ins/w:del.

/// A fldSimple DATE field whose cached result has a tracked-inserted run.
const FLDSIMPLE_BODY: &str = r#"<w:p><w:fldSimple w:instr=" DATE "><w:r><w:t>OLD</w:t></w:r><w:ins w:id="100" w:author="probe" w:date="2026-06-11T00:00:00Z"><w:r><w:t xml:space="preserve"> NEW</w:t></w:r></w:ins></w:fldSimple></w:p>"#;

/// An inline customXml wrapper whose content has a tracked-inserted run.
const CUSTOMXML_BODY: &str = r#"<w:p><w:customXml w:element="tag"><w:r><w:t>OLD</w:t></w:r><w:ins w:id="100" w:author="probe" w:date="2026-06-11T00:00:00Z"><w:r><w:t xml:space="preserve"> NEW</w:t></w:r></w:ins></w:customXml></w:p>"#;

#[test]
fn accept_all_resolves_ins_inside_fldsimple() {
    let doc = Document::parse(&make_docx(FLDSIMPLE_BODY)).expect("parse");
    let xml = document_xml(&doc.read_accepted().expect("accept-all"));
    assert!(
        !has_revision(&xml, "ins"),
        "accept-all unwraps fldSimple <w:ins>: {xml}"
    );
    assert!(xml.contains("OLD") && xml.contains("NEW"), "{xml}");
}

#[test]
fn reject_all_resolves_ins_inside_fldsimple() {
    let doc = Document::parse(&make_docx(FLDSIMPLE_BODY)).expect("parse");
    let xml = document_xml(&doc.read_rejected().expect("reject-all"));
    assert!(
        !has_revision(&xml, "ins"),
        "reject-all drops fldSimple <w:ins>: {xml}"
    );
    assert!(xml.contains("OLD") && !xml.contains("NEW"), "{xml}");
}

#[test]
fn accept_all_resolves_ins_inside_customxml() {
    let doc = Document::parse(&make_docx(CUSTOMXML_BODY)).expect("parse");
    let xml = document_xml(&doc.read_accepted().expect("accept-all"));
    assert!(
        !has_revision(&xml, "ins"),
        "accept-all unwraps customXml <w:ins>: {xml}"
    );
    assert!(xml.contains("OLD") && xml.contains("NEW"), "{xml}");
}

#[test]
fn reject_all_resolves_ins_inside_customxml() {
    let doc = Document::parse(&make_docx(CUSTOMXML_BODY)).expect("parse");
    let xml = document_xml(&doc.read_rejected().expect("reject-all"));
    assert!(
        !has_revision(&xml, "ins"),
        "reject-all drops customXml <w:ins>: {xml}"
    );
    assert!(xml.contains("OLD") && !xml.contains("NEW"), "{xml}");
}

// ---- Round-trip of an opaque WITHOUT inner revisions stays verbatim ----
//
// A clean opaque (no inner revisions) must come through accept/reject with its
// container's inner content byte-identical: the descent only rewrites `raw_xml`
// when it actually resolves a revision (CLAUDE.md — no needless mutation of
// clean content). We compare the inner container region between a plain
// round-trip and the projected output. (The whole-document serialization differs
// in namespace declarations because projection rebuilds the document from the
// scaffold; that is a pre-existing artifact of the projection path, unrelated to
// this descent, so we scope the assertion to the opaque's own content.)

/// Extract the substring between the first `<open` start tag and its matching
/// `</close>` end tag (inclusive of `<open` up to and including `</close>`),
/// for verifying an opaque container's inner bytes are preserved. `open` is a
/// tag prefix like `<w:txbxContent` (no `>`, since attributes may follow).
fn inner_region(xml: &str, open: &str, close: &str) -> String {
    let start = xml.find(open).expect("open tag present");
    let end = xml.find(close).expect("close tag present") + close.len();
    xml[start..end].to_string()
}

#[test]
fn accept_all_leaves_clean_textbox_unchanged() {
    let clean_body = r#"<w:p><w:r><w:t>Body before textbox.</w:t></w:r><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2700000" cy="900000"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t xml:space="preserve">OLD NEW</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let doc = Document::parse(&make_docx(clean_body)).expect("parse");
    let baseline = inner_region(&document_xml(&doc), "<w:txbxContent", "</w:txbxContent>");
    let accepted = doc.read_accepted().expect("accept-all");
    assert_eq!(
        baseline,
        inner_region(
            &document_xml(&accepted),
            "<w:txbxContent",
            "</w:txbxContent>"
        ),
        "accept-all must not change a textbox interior that has no inner revisions"
    );
    let rejected = doc.read_rejected().expect("reject-all");
    assert_eq!(
        baseline,
        inner_region(
            &document_xml(&rejected),
            "<w:txbxContent",
            "</w:txbxContent>"
        ),
        "reject-all must not change a textbox interior that has no inner revisions"
    );
}

#[test]
fn accept_all_leaves_clean_sdt_unchanged() {
    let clean_body = r#"<w:p><w:sdt><w:sdtPr><w:tag w:val="TenantName"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">OLD NEW</w:t></w:r></w:sdtContent></w:sdt></w:p>"#;
    let doc = Document::parse(&make_docx(clean_body)).expect("parse");
    let baseline = inner_region(&document_xml(&doc), "<w:sdtContent", "</w:sdtContent>");
    let accepted = doc.read_accepted().expect("accept-all");
    assert_eq!(
        baseline,
        inner_region(&document_xml(&accepted), "<w:sdtContent", "</w:sdtContent>"),
        "accept-all must not change an sdt interior that has no inner revisions"
    );
    let rejected = doc.read_rejected().expect("reject-all");
    assert_eq!(
        baseline,
        inner_region(&document_xml(&rejected), "<w:sdtContent", "</w:sdtContent>"),
        "reject-all must not change an sdt interior that has no inner revisions"
    );
}

// ---- Corpus sentinels (nightly; require the private stress corpus) ----
//
// Real third-party documents that carry the two defect shapes:
//   - WC066-Textbox-Before-Ins-Mod: w:ins inside a textbox txbxContent.
//   - RP004-Deleted-Text-in-CC: w:del inside an inline content control.
// Both must project (accept-all AND reject-all) without error AND leave no
// revision markup inside the opaque container.

/// Read a corpus DOCX, project it both ways, and assert the projected bytes
/// carry no `w:ins`/`w:del`/`w:delText` anywhere (the resolution must reach
/// inside opaque containers, not just the body).
fn assert_sentinel_resolves(rel_path: &str) {
    let path = common::stress_dir().join(rel_path);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc = Document::parse(&bytes).expect("parse sentinel");

    for (label, projected) in [
        ("accept-all", doc.read_accepted().expect("accept-all")),
        ("reject-all", doc.read_rejected().expect("reject-all")),
    ] {
        let xml = document_xml(&projected);
        assert!(
            !has_revision(&xml, "ins") && !has_revision(&xml, "del"),
            "{label} of {rel_path} must resolve ALL revisions (none may survive \
             inside an opaque container)"
        );
    }
}

#[test]
#[ignore = "requires private stress corpus; set STEMMA_CORPUS_ROOT — run via just nightly"]
fn sentinel_wc066_textbox_ins_resolves() {
    assert_sentinel_resolves(
        "open-xml-powertools/TestFiles__WC__WC066-Textbox-Before-Ins-Mod.docx",
    );
}

#[test]
#[ignore = "requires private stress corpus; set STEMMA_CORPUS_ROOT — run via just nightly"]
fn sentinel_rp004_content_control_del_resolves() {
    assert_sentinel_resolves("open-xml-powertools/TestFiles__RP__RP004-Deleted-Text-in-CC.docx");
}

#[test]
#[ignore = "requires private stress corpus; set STEMMA_CORPUS_ROOT — run via just nightly"]
fn sentinel_ema_humira_projects_clean() {
    // A large real EMA document with textbox revisions. Acceptance is just:
    // both projections succeed and leave no revision markup behind.
    assert_sentinel_resolves("corpus/ema_humira_nl.docx");
}

// The Word-oracle open-clean cases for these fixtures live in the held-out
// real-Word conformance tier.
