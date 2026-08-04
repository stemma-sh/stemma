//! Round-trip fidelity for documents that ALREADY CONTAIN tracked moves.
//!
//! Word's native "move" (drag a paragraph elsewhere with Track Changes on)
//! serializes as paired range bookmarks plus content containers:
//!
//! ```xml
//! <w:moveFromRangeStart w:id w:name/> <w:moveFrom …>…runs…</w:moveFrom> <w:moveFromRangeEnd w:id/>
//! <w:moveToRangeStart   w:id w:name/> <w:moveTo …>…runs…</w:moveTo>     <w:moveToRangeEnd   w:id/>
//! ```
//!
//! On import these decompose into `MoveRange` decorations (the range
//! bookmarks AND the childless container brackets) wrapping the move content,
//! which the importer must capture with their raw XML — otherwise the
//! serializer cannot rebuild them and re-materialization fails with
//! `UnsupportedEdit("decoration without raw XML cannot be serialized")`.
//!
//! These are DAILY (merge-gate) regressions: a pre-existing move must survive
//! parse → (re-materialize) → serialize, must round-trip on re-parse, and the
//! accept/reject projections must read like the move applied / not-yet-applied.
//! The Word-oracle conformance counterpart lives in the held-out real-Word
//! conformance tier.

use std::io::{Cursor, Read, Write};

use stemma::ExportOptions;
use stemma::api::Document;
use zip::ZipWriter;
use zip::write::FileOptions;

// ── DOCX builder helpers (same minimal-package style as the other hermetic
//    tracked-change tests; kept local so this file is self-contained) ────────

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const WORD_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

fn build_docx(document_xml: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(WORD_RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();

    let cursor = zip.finish().unwrap();
    cursor.into_inner()
}

fn wrap_body(body_content: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
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

/// A document whose body contains one real Word-style move: the moved text
/// "the indemnity clause" leaves its original spot (`moveFrom`, between the
/// `move7` range bookmarks) and reappears later (`moveTo`, same `move7` name).
/// Surrounding paragraphs are untouched anchors.
fn move_bearing_body() -> &'static str {
    r#"
    <w:p>
      <w:r><w:t>Section 1 is unchanged.</w:t></w:r>
    </w:p>
    <w:p>
      <w:moveFromRangeStart w:id="200" w:name="move7"/>
      <w:moveFrom w:id="201" w:author="Reviewer" w:date="2025-02-01T09:00:00Z">
        <w:r><w:t>the indemnity clause</w:t></w:r>
      </w:moveFrom>
      <w:moveFromRangeEnd w:id="200"/>
    </w:p>
    <w:p>
      <w:r><w:t>Section 2 is unchanged.</w:t></w:r>
    </w:p>
    <w:p>
      <w:moveToRangeStart w:id="202" w:name="move7"/>
      <w:moveTo w:id="203" w:author="Reviewer" w:date="2025-02-01T09:00:00Z">
        <w:r><w:t>the indemnity clause</w:t></w:r>
      </w:moveTo>
      <w:moveToRangeEnd w:id="202"/>
    </w:p>"#
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut z = zip::ZipArchive::new(Cursor::new(docx.to_vec())).expect("zip");
    let mut f = z.by_name("word/document.xml").expect("document.xml");
    let mut xml = String::new();
    f.read_to_string(&mut xml).expect("read");
    xml
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// Iterate the start-tags of `tag` (exactly — `<w:moveFrom` matches the
/// container but NOT `<w:moveFromRangeStart`, because the next char is a name
/// char, not `>`/whitespace/`/`). Yields `true` for a self-closed/empty tag
/// and `false` for an open tag that nests children.
fn move_container_tags(xml: &str, tag: &str) -> Vec<bool> {
    let open = format!("<{tag}");
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = xml[search_from..].find(&open) {
        let start = search_from + rel;
        // The char immediately after the matched prefix must NOT be a name
        // char, else this is a longer element (e.g. moveFromRangeStart).
        let after = start + open.len();
        let boundary = xml[after..].chars().next();
        let is_exact = matches!(
            boundary,
            Some(' ') | Some('>') | Some('/') | Some('\t') | Some('\n')
        );
        if let Some(close_rel) = xml[start..].find('>') {
            let tag_end = start + close_rel;
            if is_exact {
                let self_closed = tag_end > 0 && bytes[tag_end - 1] == b'/';
                out.push(self_closed);
            }
            search_from = tag_end + 1;
        } else {
            break;
        }
    }
    out
}

/// Total number of `<w:moveFrom>`/`<w:moveTo>` container elements (open or
/// self-closed), excluding the `…RangeStart`/`…RangeEnd` bookmark markers.
fn move_container_count(xml: &str, tag: &str) -> usize {
    move_container_tags(xml, tag).len()
}

// ════════════════════════════════════════════════════════════════════════════
// (a) parse → re-materialize → serialize SUCCEEDS (no UnsupportedEdit).
// ════════════════════════════════════════════════════════════════════════════

/// A freshly-parsed document re-zips its cached package, so the bug only fires
/// once an operation forces the body to be re-materialized from the IR. The
/// accept/reject projections are exactly that path. Both must serialize
/// without the `UnsupportedEdit("decoration without raw XML …")` error.
#[test]
fn spec_move_roundtrip_serialize_succeeds_after_rematerialize() {
    let docx = build_docx(&wrap_body(move_bearing_body()));
    let doc = Document::parse(&docx).expect("parse move-bearing doc");

    // Force IR -> XML re-materialization via both projections.
    let accepted = doc
        .read_accepted()
        .expect("accept-all projection must materialize a move-bearing doc");
    let rejected = doc
        .read_rejected()
        .expect("reject-all projection must materialize a move-bearing doc");

    // Gate the serialized bytes through the blocking OOXML linker, so a
    // malformed (e.g. orphaned-container) re-materialization is caught here.
    let opts = ExportOptions {
        validator_level: stemma::ValidatorLevel::Blocking,
        ..ExportOptions::default()
    };
    accepted
        .serialize(&opts)
        .expect("accept-all serialize must succeed (was UnsupportedEdit)");
    rejected
        .serialize(&opts)
        .expect("reject-all serialize must succeed (was UnsupportedEdit)");
}

// ════════════════════════════════════════════════════════════════════════════
// (b) A full resolution settles the move AS A UNIT (§17.13.5.21–.28): the
//     content lands per the disposition and EVERY piece of move markup — the
//     moveFrom/moveTo containers and the range bookmarks that pair them —
//     leaves the document. Word writes no tracked-change markup into a
//     resolved document; leaving the bookmarks would re-import as a pending
//     half-move that no further resolution can clear.
//
//     NOTE: an earlier revision of this test asserted the OPPOSITE (markers
//     survive both projections) — that was a characterization of the
//     half-resolution bug, not the domain rule.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn spec_move_roundtrip_projections_clear_all_move_markup() {
    let docx = build_docx(&wrap_body(move_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");

    // accept-all = the move is applied: source removed, destination kept.
    let accepted = doc.read_accepted().expect("accept");
    let acc_xml = document_xml_of(&accepted.serialize(&ExportOptions::default()).unwrap());

    assert!(
        acc_xml.contains("the indemnity clause"),
        "accept-all must keep the moved text at the destination (body:\n{acc_xml})"
    );

    // reject-all = the move is undone: source kept, destination removed.
    let rejected = doc.read_rejected().expect("reject");
    let rej_xml = document_xml_of(&rejected.serialize(&ExportOptions::default()).unwrap());
    assert!(
        rej_xml.contains("the indemnity clause"),
        "reject-all must keep the moved text at the source (body:\n{rej_xml})"
    );

    for xml in [&acc_xml, &rej_xml] {
        for marker in [
            "moveFromRangeStart",
            "moveFromRangeEnd",
            "moveToRangeStart",
            "moveToRangeEnd",
        ] {
            assert_eq!(
                count(xml, marker),
                0,
                "a full resolution removes every move range bookmark; <w:{marker}> must not \
                 survive (body:\n{xml})"
            );
        }
        assert_eq!(
            move_container_count(xml, "w:moveFrom") + move_container_count(xml, "w:moveTo"),
            0,
            "a full resolution removes every moveFrom/moveTo container (body:\n{xml})"
        );
    }

    // The resolved outputs re-import with nothing pending — the observable
    // contract ("after resolution, every marker is gone").
    for projected in [&accepted, &rejected] {
        let bytes = projected.serialize(&ExportOptions::default()).unwrap();
        let reparsed = Document::parse(&bytes).expect("re-parse resolved output");
        assert_eq!(
            stemma::enumerate_revisions(&reparsed.snapshot().canonical).len(),
            0,
            "a fully-resolved document must re-import with zero pending revisions"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// (c) The re-materialized output re-parses, and re-parsing is stable
//     (parse → project → serialize → parse → project → serialize is idempotent
//     at the text-reading level).
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn spec_move_roundtrip_reparses_stably() {
    let docx = build_docx(&wrap_body(move_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");

    let accepted_bytes = doc
        .read_accepted()
        .expect("accept")
        .serialize(&ExportOptions::default())
        .expect("serialize accept");

    // The serialized accept-all body must itself parse cleanly…
    let reparsed = Document::parse(&accepted_bytes).expect("re-parse of serialized output");
    // …and re-reading it must give the same text as the first accept-all read
    //   (idempotence: the move is already applied, nothing left to resolve).
    assert_eq!(
        reparsed.to_text(),
        doc.read_accepted().expect("accept").to_text(),
        "re-parse of the accept-all body must read identically"
    );

    // And serializing the re-parsed doc again must still succeed.
    reparsed
        .read_accepted()
        .expect("re-accept")
        .serialize(&ExportOptions::default())
        .expect("second-generation serialize must still succeed");
}

// ════════════════════════════════════════════════════════════════════════════
// (d) Fidelity invariants: reject reads like the pre-move baseline, accept
//     reads like the post-move target, and full resolution removes the move
//     operation's now-settled range inventory.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn spec_move_roundtrip_fidelity_invariants() {
    let docx = build_docx(&wrap_body(move_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");

    // Reject-all = baseline (before the move was made): the moved text sits at
    // its ORIGINAL location (between Section 1 and Section 2) and is absent
    // from the destination. The destination *paragraph* still exists (its
    // paragraph mark was not tracked for deletion), so it reads as a trailing
    // empty block.
    let reject_text = doc.read_rejected().expect("reject").to_text();
    assert_eq!(
        reject_text,
        "Section 1 is unchanged.\n\nthe indemnity clause\n\nSection 2 is unchanged.\n\n",
        "reject-all must read like the pre-move baseline (source kept, destination emptied)"
    );

    // Accept-all = target (after the move is applied): the moved text is gone
    // from the source (its paragraph survives as an empty block) and present at
    // the destination.
    let accept_text = doc.read_accepted().expect("accept").to_text();
    assert_eq!(
        accept_text,
        "Section 1 is unchanged.\n\n\n\nSection 2 is unchanged.\n\nthe indemnity clause",
        "accept-all must read like the post-move target (source emptied, destination kept)"
    );

    // A full resolution settles the move as a unit: no move markup survives
    // either projection (see spec_move_roundtrip_projections_clear_all_move_markup
    // for the full inventory assertions). An earlier revision of this test
    // asserted the markers survive — that encoded the half-resolution bug.
    let acc_xml = document_xml_of(
        &doc.read_accepted()
            .unwrap()
            .serialize(&ExportOptions::default())
            .unwrap(),
    );
    let rej_xml = document_xml_of(
        &doc.read_rejected()
            .unwrap()
            .serialize(&ExportOptions::default())
            .unwrap(),
    );
    for marker in [
        "moveFromRangeStart",
        "moveFromRangeEnd",
        "moveToRangeStart",
        "moveToRangeEnd",
    ] {
        assert!(
            count(&acc_xml, marker) == 0 && count(&rej_xml, marker) == 0,
            "range marker <w:{marker}> must be resolved away by both projections"
        );
    }
}
