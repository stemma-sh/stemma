//! Spec: full resolution descends into the glossary document.
//!
//! The glossary document (ECMA-376 §17.12: building blocks / AutoText) can
//! legally carry tracked revisions inside its doc-part bodies (`CT_Body`). The
//! engine does not expose glossary content in the editable IR, but Word's
//! document-level Accept-All and Reject-All resolve glossary revisions. The
//! shared byte kernel therefore resolves this opaque package part in both the
//! archive and model projection paths.
//!
//! `preflight_scan` still reports the family distinctly; successful full
//! resolution returns zero unresolved glossary revisions. Selective resolution
//! does not address glossary interiors because they have no engine identity.
//!
//! Daily tier: synthesized in-memory DOCX, no corpus, no real-Word oracle.

use std::io::{Read, Write};

use stemma::ExportOptions;
use stemma::Resolution;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::normalize::{normalize_docx, preflight_scan, reject_all_docx};

const GLOSSARY_PART: &str = "word/glossary/document.xml";
const GLOSSARY_HEADER_PART: &str = "word/glossary/header1.xml";

/// A glossary document with one building block whose doc-part body carries one
/// inserted run and one deleted run. The engine's revision tally counts the
/// `w:del` and its inner `w:delText` as separate units — the SAME element-level
/// convention it uses for `revisions_resolved` and preflight `totals` — so this
/// body discloses as 3 counted revisions (ins + del + delText), keeping the
/// unresolved count comparable to the resolved count in the same report.
const GLOSSARY_WITH_REVISIONS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:glossaryDocument xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docParts><w:docPart><w:docPartPr><w:name w:val="Block"/><w:category><w:name w:val="General"/><w:gallery w:val="quickParts"/></w:category><w:types><w:type w:val="bbPlcHdr"/></w:types><w:guid w:val="{A1B2C3D4-0000-0000-0000-000000000001}"/></w:docPartPr><w:docPartBody><w:p><w:r><w:t xml:space="preserve">Base </w:t></w:r><w:ins w:id="10" w:author="R" w:date="2024-01-01T00:00:00Z"><w:r><w:t>kept</w:t></w:r></w:ins><w:del w:id="11" w:author="R" w:date="2024-01-01T00:00:00Z"><w:r><w:delText>gone</w:delText></w:r></w:del></w:p></w:docPartBody></w:docPart></w:docParts></w:glossaryDocument>"#;

/// The main document body carries its OWN tracked change (an inserted run) so
/// the tests can prove main-document resolution still happens with the glossary.
const DOCUMENT_BODY: &str = r#"<w:p><w:r><w:t xml:space="preserve">Main </w:t></w:r><w:ins w:id="1" w:author="R" w:date="2024-01-01T00:00:00Z"><w:r><w:t>added</w:t></w:r></w:ins></w:p><w:sectPr/>"#;

/// Build a DOCX. When `with_glossary` is true, include `word/glossary/document.xml`
/// plus its content-type Override and the `glossaryDocument` relationship, so
/// both `Document::parse` and `DocxArchive::read` discover it exactly as they
/// would for a real Word file.
fn docx(with_glossary: bool) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{DOCUMENT_BODY}</w:body></w:document>"#
    );

    let glossary_override = if with_glossary {
        r#"<Override PartName="/word/glossary/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.glossary+xml"/>"#
    } else {
        ""
    };
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{glossary_override}</Types>"#
    );

    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    let glossary_rel = if with_glossary {
        r#"<Relationship Id="rId100" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/glossaryDocument" Target="glossary/document.xml"/>"#
    } else {
        ""
    };
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{glossary_rel}</Relationships>"#
    );

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
        if with_glossary {
            zip.start_file(GLOSSARY_PART, opts).unwrap();
            zip.write_all(GLOSSARY_WITH_REVISIONS.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Add a revision-bearing sub-story reached through the glossary's own
/// relationships part. Word permits glossary building blocks to carry their
/// own headers and related stories; full resolution must not stop at the
/// glossary main part.
fn docx_with_glossary_header() -> Vec<u8> {
    let archive = DocxArchive::read(&docx(true)).expect("base archive");
    let mut archive = archive.clone();
    archive.upsert(
        "word/glossary/_rels/document.xml.rels",
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#.to_vec(),
    );
    archive.upsert(
        GLOSSARY_HEADER_PART,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:ins w:id="20" w:author="R"><w:r><w:t>header-added</w:t></w:r></w:ins><w:del w:id="21" w:author="R"><w:r><w:delText>header-removed</w:delText></w:r></w:del></w:p></w:hdr>"#.to_vec(),
    );
    archive.write().expect("write archive")
}

fn part_bytes(bytes: &[u8], part: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut f = zip.by_name(part).unwrap();
    let mut v = Vec::new();
    f.read_to_end(&mut v).unwrap();
    v
}

// ── (a) accept-all: glossary and main both resolve ───────────────────────────

#[test]
fn accept_all_resolves_glossary_and_counts_it() {
    let bytes = docx(true);
    let archive = DocxArchive::read(&bytes).expect("read");
    let (out, result) = normalize_docx(&archive).expect("accept");

    let glossary = String::from_utf8(out.get(GLOSSARY_PART).unwrap().to_vec()).unwrap();
    assert!(!glossary.contains("<w:ins") && !glossary.contains("<w:del"));
    assert!(glossary.contains("kept") && !glossary.contains("gone"));
    assert_eq!(
        result.unresolved_glossary_revisions, 0,
        "successful accept leaves no glossary revision pending: {result:?}"
    );

    // The MAIN document's revision still resolves (accept keeps inserted text,
    // strips the wrapper).
    let document = String::from_utf8(out.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        !document.contains("<w:ins") && document.contains("added"),
        "main-document revision must resolve on accept: {document}"
    );
    assert!(
        result.revisions_resolved >= 1,
        "main-document revision must count into revisions_resolved: {result:?}"
    );
    assert!(
        result.parts_normalized.iter().any(|p| p == GLOSSARY_PART),
        "glossary must be reported normalized: {:?}",
        result.parts_normalized
    );
}

// ── (b) reject-all twin ──────────────────────────────────────────────────────

#[test]
fn reject_all_resolves_glossary_and_counts_it() {
    let bytes = docx(true);
    let archive = DocxArchive::read(&bytes).expect("read");
    let (out, result) = reject_all_docx(&archive).expect("reject");

    let glossary = String::from_utf8(out.get(GLOSSARY_PART).unwrap().to_vec()).unwrap();
    assert!(!glossary.contains("<w:ins") && !glossary.contains("<w:del"));
    assert!(!glossary.contains("kept") && glossary.contains("gone"));
    assert_eq!(
        result.unresolved_glossary_revisions, 0,
        "successful reject leaves no glossary revision pending: {result:?}"
    );

    // The MAIN document's revision still resolves (reject drops inserted text).
    let document = String::from_utf8(out.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        !document.contains("<w:ins") && !document.contains("added"),
        "main-document insertion must be dropped on reject: {document}"
    );
}

// ── (c) preflight reports glossary revisions distinctly ──────────────────────

#[test]
fn preflight_reports_glossary_revisions_distinctly() {
    let bytes = docx(true);
    let archive = DocxArchive::read(&bytes).expect("read");
    let report = preflight_scan(&archive).expect("preflight");

    // Exactly one glossary part, carrying the two revisions.
    assert_eq!(
        report.glossary.len(),
        1,
        "one glossary part must be reported: {:?}",
        report.glossary
    );
    let g = &report.glossary[0];
    assert_eq!(g.part, GLOSSARY_PART, "glossary part path: {g:?}");
    assert_eq!(g.revisions.ins, 1, "one w:ins in glossary: {g:?}");
    assert_eq!(g.revisions.del, 1, "one w:del in glossary: {g:?}");
    assert_eq!(g.revisions.del_text, 1, "one w:delText in glossary: {g:?}");
    assert_eq!(
        g.revisions.total(),
        3,
        "glossary revisions total in the engine's element-tally units: {g:?}"
    );

    // The glossary revisions are NOT folded into the resolvable totals — those
    // reflect only the main document's own revision (one w:ins).
    assert_eq!(
        report.totals.revisions.ins, 1,
        "totals must count only the main-document (resolvable) w:ins: {:?}",
        report.totals
    );
    assert_eq!(
        report.totals.revisions.total(),
        1,
        "glossary revisions must not inflate resolvable totals: {:?}",
        report.totals
    );
}

// ── (d) no glossary → count is zero, report empty ────────────────────────────

#[test]
fn no_glossary_means_zero_disclosure() {
    let bytes = docx(false);
    let archive = DocxArchive::read(&bytes).expect("read");

    let report = preflight_scan(&archive).expect("preflight");
    assert!(
        report.glossary.is_empty(),
        "no glossary part → empty glossary report: {:?}",
        report.glossary
    );

    let (_out, accept) = normalize_docx(&archive).expect("accept");
    assert_eq!(
        accept.unresolved_glossary_revisions, 0,
        "no glossary → zero unresolved: {accept:?}"
    );
    let (_out, reject) = reject_all_docx(&archive).expect("reject");
    assert_eq!(reject.unresolved_glossary_revisions, 0);
}

#[test]
fn full_resolution_descends_into_glossary_substories() {
    let bytes = docx_with_glossary_header();
    let archive = DocxArchive::read(&bytes).expect("archive");

    let preflight = preflight_scan(&archive).expect("preflight");
    assert!(
        preflight
            .glossary
            .iter()
            .any(|part| part.part == GLOSSARY_HEADER_PART),
        "glossary header revisions must be disclosed: {:?}",
        preflight.glossary
    );

    let (accepted, _) = normalize_docx(&archive).expect("accept");
    let accepted = String::from_utf8(accepted.get(GLOSSARY_HEADER_PART).unwrap().to_vec()).unwrap();
    assert!(!accepted.contains("<w:ins") && !accepted.contains("<w:del"));
    assert!(accepted.contains("header-added") && !accepted.contains("header-removed"));

    let (rejected, _) = reject_all_docx(&archive).expect("reject");
    let rejected = String::from_utf8(rejected.get(GLOSSARY_HEADER_PART).unwrap().to_vec()).unwrap();
    assert!(!rejected.contains("<w:ins") && !rejected.contains("<w:del"));
    assert!(!rejected.contains("header-added") && rejected.contains("header-removed"));
}

// ── Model-path parity: same resolved glossary bytes as wire path ──────────────

#[test]
fn model_path_resolves_glossary_both_directions() {
    let bytes = docx(true);
    let archive = DocxArchive::read(&bytes).expect("archive");

    for (label, resolution, wire) in [
        (
            "accept",
            Resolution::AcceptAll,
            normalize_docx(&archive).expect("wire accept").0,
        ),
        (
            "reject",
            Resolution::RejectAll,
            reject_all_docx(&archive).expect("wire reject").0,
        ),
    ] {
        let doc = Document::parse(&bytes).expect("parse");
        let resolved = doc.project(resolution).expect("project");
        let model_bytes = resolved
            .serialize(&ExportOptions::default())
            .expect("serialize");
        let model_glossary = part_bytes(&model_bytes, GLOSSARY_PART);
        assert_eq!(
            model_glossary,
            wire.get(GLOSSARY_PART).unwrap(),
            "model and wire glossary resolution must agree for {label}"
        );
    }
}
