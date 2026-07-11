//! A bookmark pair whose START is base/live and whose END rides a
//! target-origin block (a `w:moveTo` clone or a block-level insertion that kept
//! the base id) must serialize with the pair INTACT — through a plain round-trip
//! and through every selective/accept/reject resolution.
//!
//! Root cause (fixed): the per-part bookmark id policy
//! (`serialize::BookmarkScan`) dropped a target-origin half whose partner did
//! not materialize as ANOTHER target half — but here the partner is a
//! BASE-origin half of the same id (the range straddles the origin boundary).
//! Dropping the target end left a lone `bookmarkStart` and
//! `enforce_story_bookmark_integrity` refused the document ("serialization
//! introduced unpaired bookmarks … torn across emission paths"). The wild
//! sessions reached this via `move_range` (the end rode a moveTo clone) then a
//! selective reject of an inserted paragraph mark that merged into that clone;
//! this synthetic fixture crafts the emission-time state directly — a base
//! bookmarkStart plus a bookmarkEnd inside a `w:moveTo` clone paragraph with no
//! base copy of the end — and asserts the pair balances under every subset×
//! action resolution (the leg that routes through the scaffold-merge emission).
//! Corpus-free, daily tier.

use std::collections::HashSet;
use std::io::Read;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::tracked_model::{ResolveSelectionAction, enumerate_revisions};
use zip::ZipArchive;

fn pack(body_inner_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner_xml}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
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

/// The wild shape, crafted directly: `_GoBack`-style bookmark whose START is
/// base/live (para 1) and whose END rides a `w:moveTo` clone paragraph (para 4,
/// a target-origin Inserted block) that KEPT the base id 0 — with NO base copy
/// of the end (the moveFrom source paragraph carries no bookmark). A separate
/// tracked insertion (id 30) elsewhere forces every resolution through the
/// scaffold-merge emission (`serialize_canonical_docx`), where the per-part
/// bookmark id policy used to drop the lone target end as if unpaired.
fn straddling_docx() -> Vec<u8> {
    let a = r#"w:author="Editor" w:date="2026-07-10T00:00:00Z""#;
    let body = format!(
        r#"<w:p>
            <w:bookmarkStart w:id="0" w:name="_GoBack"/>
            <w:r><w:t>alpha start </w:t></w:r>
            <w:ins w:id="30" {a}><w:r><w:t>extra</w:t></w:r></w:ins>
        </w:p>
        <w:p><w:r><w:t>middle</w:t></w:r></w:p>
        <w:moveFromRangeStart w:id="10" w:name="mv1" {a}/>
        <w:p>
            <w:pPr><w:rPr><w:moveFrom w:id="11" {a}/></w:rPr></w:pPr>
            <w:moveFrom w:id="12" {a}><w:r><w:t>moved para</w:t></w:r></w:moveFrom>
        </w:p>
        <w:moveFromRangeEnd w:id="10"/>
        <w:moveToRangeStart w:id="10" w:name="mv1" {a}/>
        <w:p>
            <w:pPr><w:rPr><w:moveTo w:id="13" {a}/></w:rPr></w:pPr>
            <w:moveTo w:id="14" {a}><w:r><w:t>moved para</w:t></w:r></w:moveTo>
            <w:bookmarkEnd w:id="0"/>
        </w:p>
        <w:moveToRangeEnd w:id="10"/>
        <w:p><w:r><w:t>tail</w:t></w:r></w:p>"#
    );
    pack(&body)
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = ZipArchive::new(std::io::Cursor::new(docx)).expect("open DOCX zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("word/document.xml present");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("read document.xml");
    out
}

fn marker_ids(xml: &str, open: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = xml[i..].find(open) {
        let s = i + p;
        let e = xml[s..].find("/>").map(|e| s + e).unwrap_or(xml.len());
        if let Some(idp) = xml[s..e].find("w:id=\"") {
            let rest = &xml[s + idp + 6..e];
            if let Some(q) = rest.find('"') {
                out.push(rest[..q].to_string());
            }
        }
        i = e;
    }
    out.sort();
    out
}

/// Serialize (blocking guard on) and assert every range family's start/end id
/// multisets are equal — the pair is intact.
fn assert_balanced(doc: &Document, ctx: &str) {
    let bytes = doc
        .serialize(&ExportOptions::default())
        .unwrap_or_else(|e| panic!("{ctx}: serialize refused: {e:?}"));
    let xml = document_xml_of(&bytes);
    for (open_s, open_e) in [
        ("<w:bookmarkStart ", "<w:bookmarkEnd "),
        ("<w:commentRangeStart ", "<w:commentRangeEnd "),
        ("<w:permStart ", "<w:permEnd "),
    ] {
        assert_eq!(
            marker_ids(&xml, open_s),
            marker_ids(&xml, open_e),
            "{ctx}: unbalanced {open_s}/{open_e}\n{xml}"
        );
    }
    Document::parse(&bytes).unwrap_or_else(|e| panic!("{ctx}: re-import refused: {e:?}"));
}

#[test]
fn straddling_bookmark_round_trips_paired() {
    // Sanity: the crafted fixture itself imports and serializes with the pair
    // intact (the scaffold-merge drop is exercised by the resolution sweep below).
    let doc = Document::parse(&straddling_docx()).expect("parse");
    assert_balanced(&doc, "round-trip");
}

#[test]
fn straddling_bookmark_balanced_under_every_subset_resolution() {
    let doc = Document::parse(&straddling_docx()).expect("parse");
    let ids: Vec<u32> = {
        let mut s: Vec<u32> = enumerate_revisions(&doc.snapshot().canonical)
            .into_iter()
            .map(|r| r.revision_id)
            .collect();
        s.sort_unstable();
        s.dedup();
        s
    };
    assert!(!ids.is_empty(), "fixture must carry tracked changes");

    // Every single-id subset × {Accept, Reject}, plus AcceptAll / RejectAll:
    // the emitted bookmark pair must balance (belt-and-braces around the
    // enforce_story_bookmark_integrity refusal staying silent).
    for &id in &ids {
        for action in [
            ResolveSelectionAction::Accept,
            ResolveSelectionAction::Reject,
        ] {
            let projected = doc
                .project(stemma::Resolution::Selective {
                    ids: HashSet::from([id]),
                    action,
                })
                .unwrap_or_else(|e| panic!("project id={id} {action:?}: {e:?}"));
            assert_balanced(&projected, &format!("selective id={id} {action:?}"));
        }
    }
    for (label, res) in [
        ("AcceptAll", stemma::Resolution::AcceptAll),
        ("RejectAll", stemma::Resolution::RejectAll),
    ] {
        let projected = doc
            .project(res)
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert_balanced(&projected, label);
    }
}
